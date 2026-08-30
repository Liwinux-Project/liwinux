//! `liw video` — what decodes video, where, and what it costs.
//!
//! This command CHANGES NOTHING. It reads the codec configuration on both
//! sides of the container boundary and reports where they fail to meet.
//!
//! Two things here are MEASURED rather than assumed:
//!
//!   * host capability, by actually decoding a clip through the hardware
//!     decoder — "ffmpeg has the decoder compiled in" is not the same claim as
//!     "this GPU decodes this format";
//!   * the cost of software decode, by decoding the same clip both ways and
//!     comparing CPU time.
//!
//! It works whether or not Android is running. With a live session the codec
//! configuration is read from the mounted rootfs; with the session down it is
//! read straight out of the images. The report says which it used, because
//! they are not equally strong evidence.

use anyhow::{Context, Result};
use liw_core::video::{self, Codec, CodecKind, HostCaps, Severity, Source};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const ROOTFS: &str = "/var/lib/waydroid/rootfs";
const IMAGES: &str = "/var/lib/waydroid/images";

// ---------------------------------------------------------------------------
// Reading the guest
// ---------------------------------------------------------------------------

/// Which image a guest path lives in.
#[derive(Clone, Copy, PartialEq)]
enum Part {
    /// vendor.img, mounted at <rootfs>/vendor.
    Vendor,
    /// system.img, mounted at <rootfs>.
    System,
}

impl Part {
    fn image(self) -> PathBuf {
        let f = match self {
            Part::Vendor => "vendor.img",
            Part::System => "system.img",
        };
        Path::new(IMAGES).join(f)
    }

    /// Where a path inside the image appears on the host when mounted.
    fn mounted(self, inner: &str) -> PathBuf {
        let inner = inner.trim_start_matches('/');
        match self {
            Part::Vendor => Path::new(ROOTFS).join("vendor").join(inner),
            Part::System => Path::new(ROOTFS).join(inner),
        }
    }
}

/// Reads a file from inside the guest.
///
/// Prefers the mounted rootfs; falls back to reading the image directly with
/// `debugfs`, so the diagnosis still works with the session stopped — which is
/// exactly when someone is most likely to be debugging why video is broken.
fn read_guest(part: Part, inner: &str) -> Option<String> {
    if let Ok(s) = std::fs::read_to_string(part.mounted(inner)) {
        return Some(s);
    }
    let img = part.image();
    if !img.exists() { return None; }
    let tmp = std::env::temp_dir().join(format!(
        "liw-video-{}", inner.replace('/', "_")));
    let out = Command::new("debugfs")
        .args(["-R", &format!("dump {inner} {}", tmp.display())])
        .arg(&img)
        .output().ok()?;
    if !out.status.success() { return None; }
    let s = std::fs::read_to_string(&tmp).ok();
    let _ = std::fs::remove_file(&tmp);
    s.filter(|s| !s.is_empty())
}

/// Lists a directory inside the guest, by the same two routes.
fn list_guest(part: Part, inner: &str) -> Vec<String> {
    if let Ok(rd) = std::fs::read_dir(part.mounted(inner)) {
        return rd.filter_map(|e| Some(e.ok()?.file_name().to_string_lossy().into_owned()))
            .collect();
    }
    let img = part.image();
    if !img.exists() { return Vec::new(); }
    let Ok(out) = Command::new("debugfs")
        .args(["-R", &format!("ls -l {inner}")])
        .arg(&img)
        .output() else { return Vec::new() };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_whitespace().last())
        .filter(|n| *n != "." && *n != "..")
        .map(str::to_string)
        .collect()
}

/// Whether the guest images are readable at all, and by which route.
fn guest_source() -> Source {
    if Path::new(ROOTFS).join("vendor/etc/media_codecs.xml").exists() {
        Source::Live
    } else if Path::new(IMAGES).join("vendor.img").exists() {
        Source::Image
    } else {
        Source::None
    }
}

/// Every video codec the guest declares, from both places they are declared.
///
/// Reading only /vendor/etc misses the `c2.android.*` components entirely:
/// those live in the swcodec APEX, and they are the ones Android actually
/// prefers. Looking at the vendor file alone suggested this image had nothing
/// but the legacy `OMX.google.*` set, which was wrong.
fn guest_codecs() -> (Vec<Codec>, Vec<String>) {
    let mut all = Vec::new();

    // 1. The vendor hub and everything it pulls in.
    let hub = read_guest(Part::Vendor, "/etc/media_codecs.xml").unwrap_or_default();
    let declared = video::parse_includes(&hub);
    let present = list_guest(Part::Vendor, "/etc");
    let missing = video::missing_includes(&declared, &present);

    all.extend(video::parse_codec_xml(&hub));
    for inc in &declared {
        if let Some(x) = read_guest(Part::Vendor, &format!("/etc/{inc}")) {
            all.extend(video::parse_codec_xml(&x));
        }
    }

    // 2. The software codec APEX — where c2.android.* is declared.
    const SW: &str = "/system/apex/com.android.media.swcodec/etc/media_codecs.xml";
    if let Some(x) = read_guest(Part::System, SW) {
        all.extend(video::parse_codec_xml(&x));
    }

    all.retain(|c| c.mime.starts_with("video/"));
    all.sort_by(|a, b| a.name.cmp(&b.name));
    all.dedup_by(|a, b| a.name == b.name && a.mime == b.mime);
    (all, missing)
}

// ---------------------------------------------------------------------------
// Measuring the host
// ---------------------------------------------------------------------------

/// One format, a clip in it, and the hardware decoder that should handle it.
struct Fixture {
    mime: &'static str,
    /// File name for the temporary copy; the extension matters to the demuxer.
    file: &'static str,
    /// ffmpeg's explicit NVDEC decoder for this format.
    cuvid: &'static str,
    clip: &'static [u8],
}

/// Clips are embedded rather than generated at run time.
///
/// Generating them would make the result depend on which ENCODERS happen to be
/// installed, and a missing encoder would be indistinguishable from a missing
/// decoder. These are 320x180, half a second, a few KB each.
const FIXTURES: &[Fixture] = &[
    Fixture { mime: "video/avc", file: "h264.mp4", cuvid: "h264_cuvid", clip: include_bytes!("../probe/h264.mp4") },
    Fixture { mime: "video/hevc", file: "hevc.mp4", cuvid: "hevc_cuvid", clip: include_bytes!("../probe/hevc.mp4") },
    Fixture { mime: "video/x-vnd.on2.vp9", file: "vp9.webm", cuvid: "vp9_cuvid", clip: include_bytes!("../probe/vp9.webm") },
    Fixture { mime: "video/x-vnd.on2.vp8", file: "vp8.webm", cuvid: "vp8_cuvid", clip: include_bytes!("../probe/vp8.webm") },
    Fixture { mime: "video/av01", file: "av1.mp4", cuvid: "av1_cuvid", clip: include_bytes!("../probe/av1.mp4") },
    Fixture { mime: "video/mp4v-es", file: "mpeg4.mp4", cuvid: "mpeg4_cuvid", clip: include_bytes!("../probe/mpeg4.mp4") },
];

/// Writes a fixture clip to a temporary file.
fn stage(f: &Fixture) -> Option<PathBuf> {
    let p = std::env::temp_dir().join(format!("liw-probe-{}", f.file));
    std::fs::write(&p, f.clip).ok()?;
    Some(p)
}

/// Decodes a clip with the named decoder. True when it actually decoded.
///
/// The decoder is named EXPLICITLY (`-c:v h264_cuvid`) rather than requested as
/// a hint (`-hwaccel auto`). A hint falls back to software without saying so,
/// which would turn every probe into a pass and make the whole report a lie.
fn decodes_with(decoder: &str, clip: &Path) -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-c:v", decoder, "-i"])
        .arg(clip)
        .args(["-f", "null", "-"])
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Whether ffmpeg exists at all.
fn have_ffmpeg() -> bool {
    Command::new("ffmpeg").arg("-version")
        .stdout(Stdio::null()).stderr(Stdio::null())
        .status().map(|s| s.success()).unwrap_or(false)
}

/// Measures which formats the host decodes in hardware.
///
/// Returns `Unknown` when the probe could not run at all. That is NOT the same
/// as "decodes nothing", and conflating the two once put a false negative in
/// front of the user for every format on a machine that decodes all of them.
fn probe_host() -> HostCaps {
    if !have_ffmpeg() { return HostCaps::Unknown; }
    let mut ok = Vec::new();
    let mut ran = false;
    for f in FIXTURES {
        let Some(clip) = stage(f) else { continue };
        ran = true;
        if decodes_with(f.cuvid, &clip) {
            ok.push(f.mime.to_string());
        }
        let _ = std::fs::remove_file(&clip);
    }
    if ran { HostCaps::Measured(ok) } else { HostCaps::Unknown }
}

/// VA-API drivers installed on the host, by file name.
fn va_drivers() -> Vec<String> {
    let mut out = Vec::new();
    for dir in ["/usr/lib/dri", "/usr/lib64/dri", "/usr/lib/x86_64-linux-gnu/dri"] {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                if n.ends_with("_drv_video.so") && !out.contains(&n) {
                    out.push(n);
                }
            }
        }
    }
    out.sort();
    out
}

// ---------------------------------------------------------------------------
// CPU accounting
// ---------------------------------------------------------------------------

/// USER_HZ. The kernel reports process times in these units and it is 100 on
/// every Linux build that matters here.
const CLOCK_TICKS: f64 = 100.0;

/// CPU seconds this process's REAPED CHILDREN have used so far.
///
/// Fields 16 and 17 of /proc/self/stat are cutime and cstime. Using them means
/// the measurement needs no extra dependency and no external `time` binary,
/// and it counts every thread the child used — which is the whole point, since
/// software decoders are multi-threaded and wall time would hide that.
fn child_cpu_seconds() -> Option<f64> {
    let s = std::fs::read_to_string("/proc/self/stat").ok()?;
    // The second field is the executable name in parentheses and may itself
    // contain spaces, so fields are counted from after the closing bracket.
    let tail = &s[s.rfind(')')? + 1..];
    let f: Vec<&str> = tail.split_whitespace().collect();
    // After the ')' the first field is state, i.e. field 3. cutime is field 16,
    // so it sits at index 13 of this slice.
    let cutime: f64 = f.get(13)?.parse().ok()?;
    let cstime: f64 = f.get(14)?.parse().ok()?;
    Some((cutime + cstime) / CLOCK_TICKS)
}

/// One decode run.
///
/// CPU seconds only. Wall time was measured too at first and then never
/// reported: a decoder spread across threads finishes sooner in wall time
/// while costing more, which is the opposite of what this is asking.
struct Run {
    cpu_s: f64,
}

/// Decodes a clip and measures what it cost.
fn timed_decode(decoder: &str, clip: &Path, loops: u32) -> Option<Run> {
    let before = child_cpu_seconds()?;
    for _ in 0..loops {
        Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-c:v", decoder, "-i"])
            .arg(clip)
            .args(["-f", "null", "-"])
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
            .status().ok()?;
    }
    let after = child_cpu_seconds()?;
    Some(Run { cpu_s: after - before })
}

// ---------------------------------------------------------------------------
// liw video status
// ---------------------------------------------------------------------------

pub fn status() -> Result<()> {
    let source = guest_source();
    let (codecs, missing) = guest_codecs();
    let host = probe_host();
    let drivers = va_drivers();

    let findings = video::diagnose(source, &codecs, &host, &missing);
    let bar = "─".repeat(64);
    println!("\n  Video decode\n  {bar}");

    // --- guest ------------------------------------------------------------
    println!("\n  IN ANDROID");
    if codecs.is_empty() {
        println!("      no codec declaration could be read");
    } else {
        let dec: Vec<&Codec> = codecs.iter().filter(|c| !c.encoder).collect();
        let hw = dec.iter().filter(|c| c.kind == CodecKind::Hardware).count();
        println!("      {} video decoders, {hw} of them hardware\n", dec.len());
        for c in &dec {
            let k = match c.kind {
                CodecKind::Hardware => "hardware",
                CodecKind::Software => "CPU",
                CodecKind::Unknown => "?",
            };
            println!("        {:<28} {:<14} {k}", c.name, video::mime_label(&c.mime));
        }
    }

    // --- host -------------------------------------------------------------
    println!("\n  ON THE HOST");
    if drivers.is_empty() {
        println!("      no VA-API driver installed");
    } else {
        println!("      VA-API drivers : {}", drivers.join(", "));
    }
    match &host {
        HostCaps::Measured(m) if m.is_empty() =>
            println!("      hardware decode: none — measured, not assumed"),
        HostCaps::Measured(m) => {
            println!("      hardware decode: {}",
                     m.iter().map(|x| video::mime_label(x))
                      .collect::<Vec<_>>().join(", "));
            println!("      (measured by decoding a clip through each decoder)");
        }
        HostCaps::Unknown => {
            println!("      hardware decode: NOT DETERMINED");
            println!("      ffmpeg is needed to probe this. Nothing is claimed");
            println!("      about the host until it can be measured.");
        }
    }

    // --- findings ---------------------------------------------------------
    println!("\n  {bar}");
    for f in &findings {
        let mark = match f.severity {
            Severity::Ok => "✓",
            Severity::Gap => "!",
            Severity::Broken => "✗",
            Severity::NoLever => "-",
            Severity::Undetermined => "·",
        };
        println!("\n  {mark} {}", f.title);
        for l in wrap(&f.detail, 64) {
            println!("      {l}");
        }
    }

    let (ok, gap, broken, nolever, undet) = video::summarise(&findings);
    println!("\n  {bar}");
    println!("  {ok} fine · {gap} gaps · {broken} broken · \
              {nolever} no lever · {undet} undetermined\n");

    if gap > 0 {
        println!("  A gap is a FACT: the format is decoded on the CPU while the");
        println!("  host was measured to decode it in hardware. What that COSTS");
        println!("  is a separate question:\n");
        println!("      liw video probe\n");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// liw video probe
// ---------------------------------------------------------------------------

/// How long a clip the cost measurement uses, in seconds of video.
///
/// This length is not arbitrary. An earlier version of this measurement used
/// the same half-second fixtures as the capability probe and reported that
/// hardware decode cost EIGHT TIMES more CPU than software. That number was
/// real and completely misleading: over eight frames, building the CUDA
/// context is the entire measurement and the per-frame decode cost never
/// appears. A clip has to be long enough that decoding dominates start-up.
const CLIP_SECONDS: u32 = 10;

/// What the cost measurement encodes and then decodes both ways.
struct CostCase {
    mime: &'static str,
    encoder: &'static str,
    software: &'static str,
    cuvid: &'static str,
    ext: &'static str,
    /// Encoder flags that keep the encode fast without changing decode cost.
    flags: &'static [&'static str],
}

const COST_CASES: &[CostCase] = &[
    CostCase { mime: "video/avc", encoder: "libx264", software: "h264",
               cuvid: "h264_cuvid", ext: "mp4",
               flags: &["-preset", "veryfast", "-b:v", "2M"] },
    CostCase { mime: "video/x-vnd.on2.vp9", encoder: "libvpx-vp9", software: "vp9",
               cuvid: "vp9_cuvid", ext: "webm",
               flags: &["-speed", "8", "-deadline", "realtime", "-b:v", "2M"] },
];

/// Encodes a clip to measure against. `None` if the encoder is unavailable.
fn make_clip(c: &CostCase, w: u32, h: u32) -> Option<PathBuf> {
    let out = std::env::temp_dir()
        .join(format!("liw-cost-{}-{w}x{h}.{}", c.software, c.ext));
    let ok = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i",
               &format!("testsrc=size={w}x{h}:rate=30:duration={CLIP_SECONDS}"),
               "-c:v", c.encoder, "-pix_fmt", "yuv420p"])
        .args(c.flags)
        .args(["-y"]).arg(&out)
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
        .status().ok()?.success();
    ok.then_some(out)
}

/// CPU cost as a share of ONE core, for video played at normal speed.
///
/// This is the number that means something. "0.64 CPU-seconds" says nothing on
/// its own; "6% of one core for as long as the video plays" is a budget the
/// reader can weigh against a game that needs the rest of the machine.
fn core_share(cpu_s: f64) -> f64 {
    cpu_s / CLIP_SECONDS as f64 * 100.0
}

/// Measures what software decode costs against hardware decode, on the host.
///
/// This is a HOST measurement and it bounds rather than predicts the guest.
/// Android's `c2.android.*` decoders are a different implementation from
/// ffmpeg's, and they run behind binder and gralloc. What this number does
/// establish is the size of the prize: if software decode is cheap here,
/// building a bridge into the container cannot be worth it.
pub fn probe(loops: u32) -> Result<()> {
    if !have_ffmpeg() {
        anyhow::bail!("ffmpeg is needed to measure decode cost");
    }
    let loops = loops.max(1);
    let bar = "─".repeat(70);
    println!("\n  Decode cost — host measurement\n  {bar}");
    println!("  {CLIP_SECONDS}s of video per clip, {loops} pass(es) per decoder.\n");
    println!("  {:<8} {:>10}  {:>18}  {:>18}", "format", "size", "software", "hardware");
    println!("  {}", "─".repeat(60));

    let mut best_saving: f64 = 0.0;
    let mut measured = false;

    for c in COST_CASES {
        for (w, h) in [(1280u32, 720u32), (1920, 1080)] {
            let Some(clip) = make_clip(c, w, h) else {
                println!("  {:<8} {:>10}  {:>18}", video::mime_label(c.mime),
                         format!("{w}x{h}"), "no encoder");
                continue;
            };
            let sw = timed_decode(c.software, &clip, loops);
            let hw = if decodes_with(c.cuvid, &clip) {
                timed_decode(c.cuvid, &clip, loops)
            } else { None };
            let _ = std::fs::remove_file(&clip);

            let per = |r: &Run| r.cpu_s / loops as f64;
            match (&sw, &hw) {
                (Some(s), Some(h2)) => {
                    measured = true;
                    let (ss, hs) = (per(s), per(h2));
                    best_saving = best_saving.max(core_share(ss) - core_share(hs));
                    println!("  {:<8} {:>10}  {:>9.2}s {:>6.1}%  {:>9.2}s {:>6.1}%",
                             video::mime_label(c.mime), format!("{w}x{h}"),
                             ss, core_share(ss), hs, core_share(hs));
                }
                (Some(s), None) => {
                    measured = true;
                    let ss = per(s);
                    println!("  {:<8} {:>10}  {:>9.2}s {:>6.1}%  {:>18}",
                             video::mime_label(c.mime), format!("{w}x{h}"),
                             ss, core_share(ss), "not available");
                }
                _ => println!("  {:<8} {:>10}  {:>18}", video::mime_label(c.mime),
                              format!("{w}x{h}"), "decode failed"),
            }
        }
    }

    println!("\n  {bar}");
    if !measured {
        println!("  Nothing could be measured.\n");
        return Ok(());
    }
    println!("  The percentage is the share of ONE core used for as long as the");
    println!("  video plays. CPU seconds are totalled across threads, read from");
    println!("  the kernel's accounting of reaped children — wall time would");
    println!("  hide how many cores a software decoder spreads over.\n");
    println!("  Biggest saving hardware decode would buy: {best_saving:.1}% of one core.\n");

    // The point of measuring first is being willing to hear a no.
    if best_saving < 25.0 {
        println!("  That is a SMALL prize. Carrying frames from a host decoder");
        println!("  into the container needs a Codec2 component on the Android");
        println!("  side, a decode daemon on the host and dmabuf plumbing");
        println!("  between them. This measurement does not justify that work,");
        println!("  and no amount of wanting it to changes the number.\n");
    }
    println!("  What this does NOT measure: Android's own decoders, the binder");
    println!("  round-trip per frame, and gralloc allocation. Those need a");
    println!("  running session — `liw video verify` once it is up.\n");
    Ok(())
}

// ---------------------------------------------------------------------------
// liw video verify
// ---------------------------------------------------------------------------

/// Reads the live codec list from a running Android, for comparison.
///
/// The image says what is DECLARED. Only a running Android says what was
/// actually registered, and the two differ exactly when something is wrong.
pub async fn verify() -> Result<()> {
    let h = liw_core::helper::HelperClient::connect().await
        .context("could not reach liwd-helper")?;
    let live = h.media_codecs().await
        .context("could not read the live codec list - is the session up?")?;

    let mut names: Vec<String> = live
        .split(|c: char| !(c.is_alphanumeric() || c == '.' || c == '_'))
        .filter(|w| w.starts_with("c2.") || w.starts_with("OMX."))
        .map(str::to_string)
        .collect();
    names.sort();
    names.dedup();

    // The switch that decides whether Codec2 components exist at all.
    let ccodec = h.get_prop("debug.stagefright.ccodec").await.ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let gralloc = h.get_prop("ro.hardware.gralloc").await.unwrap_or_default();

    let (declared, _) = guest_codecs();
    let bar = "─".repeat(66);
    println!("\n  Registered vs declared\n  {bar}");
    println!("  registered by a running Android : {}", names.len());
    println!("  declared in the image           : {}", declared.len());

    // Printed BEFORE the decoder list. A list of thirteen live decoders reads
    // as good news, and on this path it is not — they register and then fail.
    if let Some(w) = video::codec2_video_risk(&gralloc, ccodec.as_deref()) {
        println!("\n  ✗ WARNING");
        for l in wrap(&w, 64) { println!("      {l}"); }
    }

    let live_video: Vec<&Codec> = declared.iter()
        .filter(|c| !c.encoder && names.iter().any(|n| *n == c.name))
        .collect();
    println!("  video decoders actually live    : {}", live_video.len());
    for c in &live_video {
        println!("      {:<28} {}", c.name, video::mime_label(&c.mime));
    }

    let short = video::explain_shortfall(&declared, &names, ccodec.as_deref());
    if short.is_empty() {
        println!("\n  ✓ Every declared component was registered.");
    }
    if short.codec2_disabled {
        println!("\n  ✗ Codec2 is switched OFF: debug.stagefright.ccodec=0");
        println!();
        for l in wrap(
            "Every c2.android.* component the image declares is absent, and \
             this one setting accounts for all of them. Waydroid sets it \
             itself when it finds no HAL gralloc: Codec2's software \
             components allocate graphic buffers through gralloc, and on the \
             fallback path that allocation fails. The switch trades newer \
             decoders for ones that work.", 64) {
            println!("      {l}");
        }
    }
    if !short.domain_gated.is_empty() {
        println!("\n  · {} component(s) gated on the device type, absent by design:",
                 short.domain_gated.len());
        for (name, dom) in &short.domain_gated {
            println!("      {name}  (domain=\"{dom}\")");
        }
        for l in wrap(
            "AOSP registers these only where the domain applies — a TV, or a \
             device with telephony. Their absence here is the design working, \
             not a fault.", 64) {
            println!("      {l}");
        }
    }
    if !short.unexplained.is_empty() {
        println!("\n  ✗ {} declared component(s) did not register, and nothing \
                  here explains why:", short.unexplained.len());
        for m in short.unexplained.iter().take(20) { println!("      {m}"); }
    }

    let dead = video::unplayable(&declared, &names);
    let lost: Vec<&str> = dead.iter().filter(|(_, g)| !g)
        .map(|(m, _)| video::mime_label(m)).collect();
    let gated: Vec<&str> = dead.iter().filter(|(_, g)| *g)
        .map(|(m, _)| video::mime_label(m)).collect();

    if !lost.is_empty() {
        println!("\n  ✗ No decoder at all for: {}", lost.join(", "));
        for l in wrap(
            "This is not a slow path, it is an absent one. Content in these \
             formats does not play. The image declares a component for each \
             of them, which is why reading the image alone does not show it.", 64) {
            println!("      {l}");
        }
    }
    if !gated.is_empty() {
        println!("\n  · No decoder for {} either.", gated.join(", "));
        for l in wrap("Its only component is gated on the device type, so the \
                       absence is expected here rather than a fault.", 64) {
            println!("      {l}");
        }
    }
    println!();
    Ok(())
}

/// Wraps text at word boundaries.
fn wrap(s: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    for w in s.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + w.chars().count() > width {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() { line.push(' '); }
        line.push_str(w);
    }
    if !line.is_empty() { out.push(line); }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_keeps_every_word() {
        let s = "the decoder runs on the cpu while the gpu block sits idle";
        assert_eq!(wrap(s, 20).join(" "), s);
    }

    #[test]
    fn wrap_respects_width() {
        assert!(wrap("aaa bbb ccc ddd eee", 7).iter().all(|l| l.chars().count() <= 7));
    }

    /// vendor.img is mounted UNDER the rootfs, system.img is the rootfs.
    /// Getting this backwards silently reads the wrong file.
    #[test]
    fn partitions_map_to_the_right_host_paths() {
        assert_eq!(Part::Vendor.mounted("/etc/media_codecs.xml"),
                   Path::new("/var/lib/waydroid/rootfs/vendor/etc/media_codecs.xml"));
        assert_eq!(Part::System.mounted("/system/apex/x/etc/media_codecs.xml"),
                   Path::new("/var/lib/waydroid/rootfs/system/apex/x/etc/media_codecs.xml"));
    }

    #[test]
    fn leading_slash_does_not_escape_the_rootfs() {
        let p = Part::Vendor.mounted("/etc/x.xml");
        assert!(p.starts_with("/var/lib/waydroid/rootfs"),
                "a leading slash must not make the join absolute: {p:?}");
    }

    #[test]
    fn images_are_named_per_partition() {
        assert!(Part::Vendor.image().ends_with("vendor.img"));
        assert!(Part::System.image().ends_with("system.img"));
    }

    #[test]
    fn missing_guest_file_is_none_not_panic() {
        assert!(read_guest(Part::Vendor, "/etc/definitely-not-here.xml").is_none());
    }

    /// The embedded clips must actually be there; an empty fixture would make
    /// every probe fail and read as "the host has no decoder".
    #[test]
    fn every_fixture_carries_a_clip() {
        assert!(!FIXTURES.is_empty());
        for f in FIXTURES {
            assert!(f.clip.len() > 512, "{} clip is too small to be real", f.mime);
        }
    }

    /// Each fixture must name a DIFFERENT format, or the probe silently tests
    /// the same one repeatedly and reports the others by accident.
    #[test]
    fn fixtures_cover_distinct_formats() {
        let mut m: Vec<&str> = FIXTURES.iter().map(|f| f.mime).collect();
        let n = m.len();
        m.sort_unstable();
        m.dedup();
        assert_eq!(m.len(), n, "duplicate MIME in the fixture table");
    }

    /// The parser must survive the real /proc/self/stat, whose second field is
    /// a parenthesised name that can itself contain spaces.
    #[test]
    fn child_cpu_time_parses_real_proc_stat() {
        assert!(child_cpu_seconds().is_some(),
                "/proc/self/stat could not be parsed");
    }

    #[test]
    fn child_cpu_time_never_goes_backwards() {
        let a = child_cpu_seconds().unwrap();
        let b = child_cpu_seconds().unwrap();
        assert!(b >= a, "CPU accounting must be monotonic: {a} then {b}");
    }
}
