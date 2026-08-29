#!/usr/bin/env bash
# liw bench - Waydroid game performance measurement (liwinux prototype)
#
#   sudo bash liw-bench.sh <package.name> [seconds]
#   sudo bash liw-bench.sh --list
#
# METHOD NOTE:
#   SurfaceFlinger keeps a ROLLING buffer of 128 frames. If the sampling
#   interval is longer than the time that buffer takes to fill, frames are
#   lost and artificial "long intervals" appear at the seam between samples.
#   This version:
#     1) measures the refresh period first and picks the sampling interval
#        ADAPTIVELY from it
#     2) computes frame-time intervals ONLY between consecutive frames within
#        the SAME snapshot -> the window-boundary artefact becomes impossible
#     3) reports capture coverage, so it is visible how representative the
#        numbers actually are
set -u
PKG="${1:-}"; DUR="${2:-60}"
OUT=/tmp/liw-bench-$$; mkdir -p "$OUT"
W() { waydroid --details-to-stdout shell -- "$@" 2>/dev/null | tr -d '\r'; }

[ "$(id -u)" = 0 ] || { echo "root required: sudo bash $0 $*"; exit 1; }
waydroid status 2>/dev/null | grep -qi "Session.*RUNNING" || { echo "the waydroid session is not running"; exit 1; }

if [ "$PKG" = "--list" ] || [ -z "$PKG" ]; then
  echo "== layers on screen =="
  W dumpsys SurfaceFlinger --list | grep -viE "^$|Dim|ColorLayer|Wallpaper|StatusBar|NavigationBar|InputMethod"
  echo; echo "Usage: sudo bash $0 <package.name> [seconds]"; exit 0
fi

# --- pick the layer: probe the candidates, take the one that returns data ---
probe() { W dumpsys SurfaceFlinger --latency "$1" 2>/dev/null \
          | awk 'NR>1 && NF==3 && $2!=0 && $2<9223372036854775807' | wc -l; }
echo "looking for the layer..."
LAYER=""
W dumpsys SurfaceFlinger --list 2>/dev/null | grep -i "$PKG" \
  | grep -viE "ActivityRecord|Dim|Blur|Wallpaper|Splash" > "$OUT/cand"
{ grep -i "SurfaceView\|BLAST" "$OUT/cand"; grep -vi "SurfaceView\|BLAST" "$OUT/cand"; } | awk '!seen[$0]++' > "$OUT/cand2"
while IFS= read -r c; do
  [ -n "$c" ] || continue
  n=$(probe "$c")
  echo "  candidate: $(echo "$c" | cut -c1-64) -> $n frames"
  [ "${n:-0}" -gt 5 ] && { LAYER="$c"; break; }
done < "$OUT/cand2"
[ -n "$LAYER" ] || { echo; echo "ERROR: no layer returned frame data. Is the game in the foreground?"; \
  W dumpsys SurfaceFlinger --list | grep -i "$PKG" | sed 's/^/  /'; exit 1; }
echo "Layer  : $LAYER"

# --- measure the refresh period, choose the sampling interval ADAPTIVELY ---
REFRESH_NS=$(W dumpsys SurfaceFlinger --latency "$LAYER" 2>/dev/null | head -1 | tr -dc '0-9')
[ -n "${REFRESH_NS:-}" ] && [ "$REFRESH_NS" -gt 1000 ] || REFRESH_NS=16666667
# time to fill the 128-frame buffer, in seconds, times a 0.45 safety margin
INTERVAL=$(awk -v r="$REFRESH_NS" 'BEGIN{v=128*(r/1e9)*0.45; if(v>1)v=1; if(v<0.08)v=0.08; printf "%.3f", v}')
HZ=$(awk -v r="$REFRESH_NS" 'BEGIN{printf "%.1f", 1e9/r}')
echo "Refresh: ${HZ} Hz  ->  sampling interval: ${INTERVAL}s (buffer: 128 frames)"
echo "Length : ${DUR}s"
echo "measuring - ACTUALLY PLAY (a menu or a still screen corrupts the numbers)..."
echo

# --- ABI and translation ---
PKGNAME=$(echo "$PKG" | grep -oE "[a-z0-9_]+\.[a-z0-9_.]+" | head -1)
if [ -n "${PKGNAME:-}" ]; then
  abi=$(W dumpsys package "$PKGNAME" | grep -oE "primaryCpuAbi=[a-z0-9_-]+" | head -1)
  gp=$(W pidof "$PKGNAME" | tail -1)
  hou="no"
  [ -n "${gp:-}" ] && [ "$(W sh -c "grep -c houdini /proc/$gp/maps" | tail -1)" != "0" ] 2>/dev/null && hou="YES"
  echo "  ABI: ${abi:-?}   houdini translation: $hou"; echo
fi

# --- sampling ---
: > "$OUT/latency.raw"
echo "t,gpu_util,vram_mb,cpu_pct,mem_used_mb,psi_some_avg10" > "$OUT/host.csv"
T0=$(date +%s.%N); prev_idle=0; prev_total=0; host_next=0; n=0
while :; do
  now=$(date +%s.%N)
  el=$(awk -v a="$now" -v b="$T0" 'BEGIN{print a-b}')
  awk -v e="$el" -v d="$DUR" 'BEGIN{exit !(e<d)}' || break
  { echo "---SAMPLE---"; W dumpsys SurfaceFlinger --latency "$LAYER"; } >> "$OUT/latency.raw"
  n=$((n+1))
  # sample the host once a second
  if awk -v e="$el" -v h="$host_next" 'BEGIN{exit !(e>=h)}'; then
    read -r g v < <(nvidia-smi --query-gpu=utilization.gpu,memory.used --format=csv,noheader,nounits 2>/dev/null | tr -d ' ' | tr ',' ' ')
    read -r _ u nn sy id rest < /proc/stat
    total=$((u+nn+sy+id)); dt=$((total-prev_total)); di=$((id-prev_idle))
    cpu=0; [ "$dt" -gt 0 ] && cpu=$(( (dt-di)*100/dt )); prev_total=$total; prev_idle=$id
    mem=$(awk '/MemAvailable/{a=$2}/MemTotal/{t=$2}END{print int((t-a)/1024)}' /proc/meminfo)
    psi=$(awk -F'[= ]' '/^some/{print $3}' /proc/pressure/memory 2>/dev/null)
    printf "%.1f,%s,%s,%s,%s,%s\n" "$el" "${g:-0}" "${v:-0}" "$cpu" "$mem" "${psi:-0}" >> "$OUT/host.csv"
    host_next=$(awk -v h="$host_next" 'BEGIN{print h+1}')
  fi
  sleep "$INTERVAL"
done
echo "  $n snapshots taken"

# --- analysis ---
python3 - "$OUT" "$REFRESH_NS" "$DUR" <<'PY'
import sys, statistics as st, csv
d, refresh_ns, dur = sys.argv[1], int(sys.argv[2]), float(sys.argv[3])
rf = refresh_ns/1e6

pairs, allframes = set(), set()
for block in open(f"{d}/latency.raw").read().split("---SAMPLE---"):
    fr = []
    for l in block.strip().splitlines()[1:]:
        p = l.split()
        if len(p) == 3:
            try: a = int(p[1])
            except ValueError: continue
            if a == 0 or a > 2**62: continue
            fr.append(a)
    fr = sorted(set(fr)); allframes.update(fr)
    # ONLY consecutive frames from within the same snapshot
    for x, y in zip(fr, fr[1:]):
        pairs.add((x, y))

dt = sorted((b-a)/1e6 for a, b in pairs)
dt = [x for x in dt if 0.05 < x < 1000]

print("="*58)
if len(dt) < 30:
    print(f"not enough data ({len(dt)} intervals). Was the game in the foreground and moving?")
else:
    def pct(p): return dt[min(int(len(dt)*p/100), len(dt)-1)]
    avg = sum(dt)/len(dt)
    span = (max(allframes)-min(allframes))/1e9 if len(allframes) > 1 else 0
    exp = span/(refresh_ns/1e9) if span else 0
    cov = 100*len(allframes)/exp if exp else 0
    print(f"FRAME TIMING   ({len(dt)} in-window intervals, {len(allframes)} distinct frames)")
    print(f"  p50            : {pct(50):7.2f} ms   -> {1000/pct(50):6.1f} FPS")
    print(f"  p95            : {pct(95):7.2f} ms")
    print(f"  p99            : {pct(99):7.2f} ms")
    print(f"  p99.9          : {pct(99.9):7.2f} ms")
    print(f"  worst          : {dt[-1]:7.2f} ms")
    print(f"  mean           : {avg:7.2f} ms   -> {1000/avg:6.1f} FPS")
    print(f"  refresh        : {rf:7.2f} ms   -> {1000/rf:6.1f} Hz")
    jank = sum(1 for x in dt if x > rf*1.5)
    j2   = sum(1 for x in dt if x > rf*2.0)
    print(f"  jank >1.5x     : {jank:5d}  (%{100*jank/len(dt):.2f})")
    print(f"  jank >2x       : {j2:5d}  (%{100*j2/len(dt):.2f})")
    print(f"  capture coverage: {cov:.0f}%  ({len(allframes)} / ~{exp:.0f} frames)")
    if cov < 60:
        print("  WARNING: low coverage - shorten the sampling interval or reduce load")

rows = list(csv.DictReader(open(f"{d}/host.csv")))
if rows:
    def col(k):
        out=[]
        for r in rows:
            try: out.append(float(r[k]))
            except (TypeError, ValueError): pass
        return out
    print()
    print("HOST RESOURCE USE")
    for lab, k, u in [("GPU","gpu_util","%"), ("VRAM","vram_mb","MB"),
                      ("CPU (system)","cpu_pct","%"), ("RAM","mem_used_mb","MB"),
                      ("mem.pressure","psi_some_avg10","")]:
        c = col(k)
        if c: print(f"  {lab:<14}: mean {st.mean(c):8.1f}{u}   peak {max(c):8.1f}{u}")
print("="*58)
print(f"raw data: {d}/latency.raw , {d}/host.csv")
PY
