#!/usr/bin/env bash
# Try turning Codec2 back on, reversibly.
#
#   sudo bash try-codec2.sh status
#   sudo bash try-codec2.sh enable    # back up, set ccodec=1, restart, verify
#   sudo bash try-codec2.sh revert    # put the backup back and restart
#
# WHY THIS EXISTS
#   Waydroid sets debug.stagefright.ccodec=0 in make_base_props() whenever it
#   finds no HAL gralloc (tools/helpers/lxc.py). With the switch off, Codec2
#   never starts and only the legacy OMX.google.* components register. On this
#   machine that leaves six video decoders instead of fourteen, and no decoder
#   at all for AV1 or MPEG-2 — those formats do not play.
#
# WHY EDITING waydroid_base.prop IS THE RIGHT LEVER
#   lxc.py has an override that reads the HOST's debug.stagefright.ccodec via
#   getprop. A Linux host has no getprop, so host_get() returns "" and that
#   override can never fire. The working path is the file: make_prop() reads
#   waydroid_base.prop verbatim at every session start and bind-mounts the
#   result into the guest. make_base_props(), which would put ccodec=0 back,
#   runs only on `waydroid init` and `waydroid upgrade`.
#
# WHAT THIS DOES NOT KNOW
#   Whether Codec2 works here. Waydroid does not disable it for fun: Codec2's
#   software components allocate graphic buffers through gralloc, and on the
#   fallback path that allocation can fail. Video may break entirely. That is
#   why the session is watched and rolled back automatically if it does not
#   come up.

set -u
MODE="${1:-status}"
WORK=/var/lib/waydroid
PROP="$WORK/waydroid_base.prop"
BKDIR="$WORK/liwinux-codec2-backup"
LATEST="$BKDIR/LATEST"
KEY="debug.stagefright.ccodec"
BOOT_TIMEOUT=180

S()  { echo; echo "== $* =="; }
say() { printf '  %s\n' "$*"; }

need_root() {
  [ "$(id -u)" = 0 ] || { echo "root required: sudo bash $0 $MODE"; exit 1; }
}

# The user who invoked sudo. waydroid session commands must not run as root.
as_user() {
  local u uid
  u="${SUDO_USER:-$(logname 2>/dev/null)}"
  [ -n "$u" ] || { echo "cannot tell which user to run as; set SUDO_USER"; exit 1; }
  uid=$(id -u "$u")
  sudo -u "$u" XDG_RUNTIME_DIR="/run/user/$uid" "$@"
}

current_value() {
  grep -E "^$KEY=" "$PROP" 2>/dev/null | tail -1 | cut -d= -f2
}

# What Android actually has right now, not what the file says.
#
# Prints "?" when it could not be read rather than an empty string. An empty
# value reads as "not set", which is a different claim from "not readable" —
# and `waydroid shell` needs root, so without it every value looks unset.
live_value() {
  local v
  if [ "$(id -u)" != 0 ]; then echo "? (needs root to read)"; return; fi
  session_up || { echo "? (session not running)"; return; }
  v=$(waydroid --details-to-stdout shell -- getprop "$KEY" 2>/dev/null | tr -d '\r' | tail -1)
  [ -n "$v" ] && echo "$v" || echo "(unset)"
}

session_up() {
  waydroid status 2>/dev/null | grep -qi "Session.*RUNNING"
}

restart_session() {
  as_user waydroid session stop >/dev/null 2>&1 || true
  sleep 3
  as_user waydroid session start >/dev/null 2>&1 &
  local b
  for i in $(seq 1 $((BOOT_TIMEOUT/5))); do
    b=$(waydroid --details-to-stdout shell -- getprop sys.boot_completed 2>/dev/null \
        | tr -d '\r' | tail -1)
    printf '\r  waiting for boot: %ds  boot_completed=%s   ' "$((i*5))" "${b:-}"
    [ "${b:-}" = "1" ] && { echo; return 0; }
    sleep 5
  done
  echo
  return 1
}

# --- what the codec list looks like right now -------------------------------
# Counts components of one family in the live codec list.
#
# Returns "?" rather than 0 when the list could not be read at all: a zero
# here would say "Codec2 did not register", which is a finding, and we would
# not have earned it.
count_components() {
  local pat="$1" dump
  dump=$(waydroid --details-to-stdout shell -- dumpsys media.player 2>/dev/null | tr -d '\r')
  case "$dump" in
    *OMX.*|*c2.*) ;;
    *) echo "?"; return ;;
  esac
  echo "$dump" | grep -oE "${pat}\.[A-Za-z0-9._]+" | sort -u | wc -l
}

report_codecs() {
  local omx c2
  omx=$(count_components "OMX")
  c2=$(count_components "c2")
  say "live $KEY : $(live_value)"
  say "OMX components : $omx"
  say "c2  components : $c2"
  if [ "$c2" = "?" ]; then
    say "-> the codec list could not be read, so nothing is claimed about Codec2"
    return
  fi
  if [ "${c2:-0}" -gt 0 ]; then
    say "-> Codec2 REGISTERED. Formats it adds over the OMX set:"
    waydroid --details-to-stdout shell -- dumpsys media.player 2>/dev/null \
      | tr -d '\r' | grep -oE "c2\.[A-Za-z0-9._]+" | sort -u | sed 's/^/       /'
  else
    say "-> Codec2 did NOT register. The switch alone was not enough."
  fi
  echo
  say "recent Codec2/CCodec errors in the log (empty is good):"
  waydroid --details-to-stdout shell -- logcat -d -t 4000 2>/dev/null | tr -d '\r' \
    | grep -iE "CCodec|Codec2|c2\.android" | grep -iE "error|fail|cannot|unable|abort" \
    | tail -8 | sed 's/^/       /' || true
}

# --- backup and restore -----------------------------------------------------
make_backup() {
  mkdir -p "$BKDIR"
  local ts dst
  ts=$(date +%Y%m%d-%H%M%S)
  dst="$BKDIR/waydroid_base.prop.$ts"
  cp -a "$PROP" "$dst" || { echo "  BACKUP FAILED - stopping"; exit 1; }
  # Verify the copy before trusting it. A backup nobody checked is not a backup.
  cmp -s "$PROP" "$dst" || { echo "  BACKUP DOES NOT MATCH - stopping"; exit 1; }
  echo "$dst" > "$LATEST"
  say "backed up -> $dst"
}

# Puts the key back without touching anything else.
#
# This is the DEFAULT way back, not a whole-file restore. If `waydroid init`
# or an upgrade has regenerated the prop file since, the backup is stale and
# copying it over would silently undo whatever else legitimately changed.
# Only this one key was altered, so only this one key is put back.
revert_key() {
  if grep -qE "^$KEY=" "$PROP"; then
    sed -i "s/^$KEY=.*/$KEY=0/" "$PROP"
  else
    echo "$KEY=0" >> "$PROP"
  fi
  [ "$(current_value)" = "0" ] || return 1
  say "$KEY set back to 0 in $PROP"
  return 0
}

# Whole-file restore. Correct right after an enable, when nothing else can
# have changed yet; risky later, which is why it is not the default.
restore_backup() {
  local src
  src=$(cat "$LATEST" 2>/dev/null)
  [ -n "$src" ] && [ -f "$src" ] || { echo "  no backup recorded in $LATEST"; return 1; }
  cp -a "$src" "$PROP" || return 1
  say "restored the whole file from $src"
  return 0
}

case "$MODE" in

status)
  S "Current state"
  say "file  $PROP"
  say "  $KEY = $(current_value)"
  say "live in Android"
  say "  $KEY = $(live_value)"
  if [ -f "$LATEST" ]; then
    say "backup on record: $(cat "$LATEST")"
  else
    say "no backup on record (nothing has been changed by this script)"
  fi
  ;;

enable)
  need_root
  [ -f "$PROP" ] || { echo "$PROP not found"; exit 1; }

  cur=$(current_value)
  if [ "$cur" = "1" ]; then
    say "$KEY is already 1 in the file; nothing to change."
    session_up && report_codecs
    exit 0
  fi

  S "1. Backup"
  make_backup

  S "2. Setting $KEY=1"
  if grep -qE "^$KEY=" "$PROP"; then
    sed -i "s/^$KEY=.*/$KEY=1/" "$PROP"
  else
    echo "$KEY=1" >> "$PROP"
  fi
  say "file now says: $KEY = $(current_value)"
  [ "$(current_value)" = "1" ] || { echo "  edit did not take - reverting"; restore_backup; exit 1; }

  S "3. Restarting the session"
  say "the prop file is read at session start, so this is required"
  if restart_session; then
    S "4. What actually happened"
    report_codecs
    echo
    say "The session came up. Whether video PLAYS is not something this"
    say "script can answer - open the game or a video and watch."
    say "If it is broken:  sudo bash $0 revert"
  else
    S "4. Boot did not complete - rolling back"
    say "Android did not reach boot_completed in ${BOOT_TIMEOUT}s."
    say "This is the failure Waydroid disables Codec2 to avoid."
    restore_backup
    say "restarting with the original setting"
    if restart_session; then
      say "recovered: the session is up again with $KEY=$(current_value)"
    else
      say "STILL not booting after the rollback - the cause is elsewhere."
      say "Check:  journalctl -u waydroid-container -n 50"
    fi
    exit 1
  fi
  ;;

revert)
  need_root
  S "Putting $KEY back to 0"
  if [ "${2:-}" = "--full" ]; then
    say "--full given: restoring the entire backed-up file"
    restore_backup || exit 1
  else
    revert_key || { echo "  could not edit $PROP"; exit 1; }
    [ -f "$LATEST" ] && say "(whole-file backup, if you want it: $(cat "$LATEST"))"
  fi
  say "$KEY = $(current_value)"
  S "Restarting the session"
  if restart_session; then
    say "session is up"
    report_codecs
  else
    say "the session did not come up; check journalctl -u waydroid-container"
    exit 1
  fi
  ;;

*)
  echo "Usage: sudo bash $0 [status|enable|revert [--full]]"
  exit 2 ;;
esac

echo
echo "  NOTE: \`waydroid init\` and \`waydroid upgrade\` regenerate"
echo "  waydroid_base.prop and will put $KEY=0 back."
