//! Video decode diagnosis.
//!
//! Android decodes video through MediaCodec. Every MediaCodec component is
//! either software (the CPU does the work) or hardware (a fixed-function block
//! does it). On a phone the hardware components come from the SoC vendor. In a
//! Waydroid container there is no SoC vendor, so what is left is whatever the
//! generic image ships — and that is software only.
//!
//! That matters beyond adverts. Every cutscene, every video splash, every
//! in-game clip and every WebView video runs on the CPU, competing with the
//! game for the very cores the game needs.
//!
//! This module answers three questions and nothing else:
//!
//!   1. What can the GUEST decode, and in software or hardware?
//!   2. What can the HOST decode in hardware?
//!   3. Where do the two fail to meet?
//!
//! It changes nothing, and it does not promise a speed-up. The size of the gap
//! is a fact; the benefit of closing it is not, until measured.
//!
//! Every function here is PURE: text in, finding out. That makes the whole
//! diagnosis testable with no GPU, no root and no running Android.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Codec components
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CodecKind {
    /// The CPU decodes. AOSP reserves `c2.android.*` and `OMX.google.*`.
    Software,
    /// A fixed-function block decodes.
    Hardware,
    /// Not a name we can classify. We say so instead of assuming.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Codec {
    pub name: String,
    pub mime: String,
    pub kind: CodecKind,
    pub encoder: bool,
    /// The `domain` attribute, when the declaration carries one.
    ///
    /// AOSP gates these components on the device type: `domain="tv"` only
    /// registers on a TV, `domain="telephony"` only where telephony exists.
    /// A gated component missing on an ordinary device is the design working,
    /// not a fault — and without this field it reads as one.
    #[serde(default)]
    pub domain: Option<String>,
}

/// Classifies a MediaCodec component by name.
///
/// This is the same rule Android itself applies in `MediaCodecInfo`: the
/// `c2.android.` and `OMX.google.` prefixes are RESERVED for the software
/// components AOSP ships. Any other vendor prefix denotes a hardware block.
///
/// The `.secure` suffix marks a DRM-capable variant of the same component and
/// says nothing about where the decoding happens, so it is stripped first.
pub fn codec_kind(name: &str) -> CodecKind {
    let n = name.trim();
    let base = n.strip_suffix(".secure").unwrap_or(n);
    if base.starts_with("c2.android.") || base.starts_with("OMX.google.") {
        CodecKind::Software
    } else if base.starts_with("c2.") || base.starts_with("OMX.") {
        CodecKind::Hardware
    } else {
        CodecKind::Unknown
    }
}

/// Strips XML comments.
///
/// This is NOT cosmetic. `media_codecs.xml` opens with a large commented-out
/// DTD that itself mentions `Include`, so any parser that skips this step
/// reads declarations that do not exist.
fn strip_comments(xml: &str) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        match rest[start..].find("-->") {
            Some(end) => rest = &rest[start + end + 3..],
            // Unterminated comment: everything after it is commented out.
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// Reads the file names in the `<Include href="..."/>` chain.
///
/// `media_codecs.xml` is a hub: it declares almost nothing itself and pulls in
/// one file per codec family. If one of those files is missing, the codecs it
/// would have declared are simply absent — with no error the user ever sees.
pub fn parse_includes(xml: &str) -> Vec<String> {
    let body = strip_comments(xml);
    let mut out = Vec::new();
    for (idx, _) in body.match_indices("<Include") {
        let tail = &body[idx..];
        let Some(end) = tail.find('>') else { continue };
        let tag = &tail[..end];
        let Some(h) = tag.find("href") else { continue };
        let after = &tag[h + 4..];
        let Some(q) = after.find(['"', '\'']) else { continue };
        let quote = after.as_bytes()[q] as char;
        let val = &after[q + 1..];
        let Some(close) = val.find(quote) else { continue };
        let name = val[..close].trim();
        if !name.is_empty() {
            out.push(name.to_string());
        }
    }
    out
}

/// Include targets that are declared but not present on disk.
///
/// The order of `declared` is preserved so the report reads in file order.
pub fn missing_includes(declared: &[String], present: &[String]) -> Vec<String> {
    declared.iter()
        .filter(|d| !present.iter().any(|p| p == *d))
        .cloned()
        .collect()
}

/// Reads codec declarations out of a `media_codecs_*.xml`.
///
/// Handles both forms AOSP allows: the one-line `<MediaCodec name= type=/>`
/// and the block form whose types are separate `<Type name=/>` children.
pub fn parse_codec_xml(xml: &str) -> Vec<Codec> {
    let body = strip_comments(xml);
    let mut out = Vec::new();
    let mut encoder_section = false;

    for raw in body.lines() {
        let line = raw.trim();
        if line.starts_with("<Encoders") { encoder_section = true; }
        if line.starts_with("</Encoders") { encoder_section = false; }
        if line.starts_with("<Decoders") { encoder_section = false; }

        if !line.starts_with("<MediaCodec ") { continue; }
        let Some(name) = attr(line, "name") else { continue };
        // An `update="true"` entry only amends an existing declaration (this is
        // how media_codecs_performance.xml works); it does not declare a codec.
        if attr(line, "update").as_deref() == Some("true") { continue; }

        // The block form carries its types as children; walking them needs the
        // whole document, so the type is recorded as empty and filled in by the
        // caller when it has the surrounding context.
        let mime = attr(line, "type").unwrap_or_default();
        out.push(Codec {
            kind: codec_kind(&name),
            encoder: encoder_section || name.contains(".encoder"),
            domain: attr(line, "domain"),
            name,
            mime,
        });
    }
    out
}

/// Pulls one attribute value out of a tag.
fn attr(tag: &str, key: &str) -> Option<String> {
    let mut from = 0usize;
    while let Some(rel) = tag[from..].find(key) {
        let at = from + rel;
        // Must be preceded by whitespace, so "type" does not match "subtype".
        let ok_before = at == 0
            || tag.as_bytes()[at - 1].is_ascii_whitespace();
        let after = &tag[at + key.len()..];
        let trimmed = after.trim_start();
        if ok_before && trimmed.starts_with('=') {
            let v = trimmed[1..].trim_start();
            let q = v.chars().next()?;
            if q == '"' || q == '\'' {
                let val = &v[1..];
                let end = val.find(q)?;
                return Some(val[..end].to_string());
            }
        }
        from = at + key.len();
    }
    None
}

// ---------------------------------------------------------------------------
// Host decode capability
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaProfile {
    pub profile: String,
    pub entrypoints: Vec<String>,
}

impl VaProfile {
    /// Whether this profile can DECODE. `VLD` is the decode entrypoint;
    /// `EncSlice` and friends are encode and must not be counted.
    pub fn decodes(&self) -> bool {
        self.entrypoints.iter().any(|e| e.contains("VLD"))
    }
}

/// Parses `vainfo` output into profiles.
///
/// Lines look like:
/// `      VAProfileH264Main               : VAEntrypointVLD`
///
/// vainfo prints one line per profile/entrypoint PAIR, so the same profile
/// appears several times and the entrypoints must be merged rather than
/// overwritten — otherwise a codec that both decodes and encodes is recorded
/// with whichever line happened to come last.
pub fn parse_vainfo(out: &str) -> Vec<VaProfile> {
    let mut acc: Vec<VaProfile> = Vec::new();
    for line in out.lines() {
        let l = line.trim();
        if !l.starts_with("VAProfile") { continue; }
        let Some((p, e)) = l.split_once(':') else { continue };
        let (p, e) = (p.trim().to_string(), e.trim().to_string());
        if e.is_empty() { continue; }
        match acc.iter_mut().find(|x| x.profile == p) {
            Some(x) => {
                if !x.entrypoints.contains(&e) { x.entrypoints.push(e); }
            }
            None => acc.push(VaProfile { profile: p, entrypoints: vec![e] }),
        }
    }
    acc
}

/// Maps a VA-API profile name to the Android MIME type it serves.
///
/// Returns `None` for profiles Android has no MIME for (JPEG, for instance),
/// so they are neither reported as a win nor as a gap.
pub fn va_profile_mime(profile: &str) -> Option<&'static str> {
    let p = profile;
    if p.starts_with("VAProfileH264") { return Some("video/avc"); }
    if p.starts_with("VAProfileHEVC") { return Some("video/hevc"); }
    if p.starts_with("VAProfileVP9") { return Some("video/x-vnd.on2.vp9"); }
    if p.starts_with("VAProfileVP8") { return Some("video/x-vnd.on2.vp8"); }
    if p.starts_with("VAProfileAV1") { return Some("video/av01"); }
    if p.starts_with("VAProfileMPEG4") { return Some("video/mp4v-es"); }
    if p.starts_with("VAProfileMPEG2") { return Some("video/mpeg2"); }
    None
}

/// The MIME types the host can decode in hardware.
pub fn host_decodable(profiles: &[VaProfile]) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for p in profiles.iter().filter(|p| p.decodes()) {
        if let Some(m) = va_profile_mime(&p.profile) {
            if !out.contains(&m) { out.push(m); }
        }
    }
    out
}

/// A human name for a MIME type.
pub fn mime_label(mime: &str) -> &str {
    match mime {
        "video/avc" => "H.264",
        "video/hevc" => "H.265 / HEVC",
        "video/x-vnd.on2.vp8" => "VP8",
        "video/x-vnd.on2.vp9" => "VP9",
        "video/av01" => "AV1",
        "video/mp4v-es" => "MPEG-4",
        "video/mpeg2" => "MPEG-2",
        "video/3gpp" => "H.263",
        other => other,
    }
}

/// Formats are ordered by how much they actually turn up in mobile video.
/// This drives report order so the important gaps are read first.
pub fn mime_weight(mime: &str) -> u8 {
    match mime {
        "video/avc" => 100,          // still the default for nearly every ad
        "video/x-vnd.on2.vp9" => 90, // what YouTube and AdMob serve
        "video/hevc" => 70,
        "video/av01" => 60,
        "video/x-vnd.on2.vp8" => 40,
        "video/mp4v-es" => 20,
        _ => 10,
    }
}

// ---------------------------------------------------------------------------
// Codec2's video path
// ---------------------------------------------------------------------------

/// gralloc implementations that are Waydroid's FALLBACK rather than a real
/// vendor HAL. lxc.py picks these when it can find no HAL gralloc, and it
/// disables Codec2 in the same branch.
const FALLBACK_GRALLOC: &[&str] = &["gbm", "default", "minigbm_gbm_mesa"];

/// Whether Codec2's video decoders can be trusted on this gralloc.
///
/// MEASURED on 2026-08-29, and it is the reason this function exists:
/// with `debug.stagefright.ccodec=1` the components register perfectly —
/// 28 of them, AV1 included — and then video does not decode. In Subway
/// Surfers an advert played its AUDIO with no picture at all, and the game
/// crashed before the advert finished. Reverting fixed it.
///
/// The split between audio and video is the whole story. Codec2's software
/// components allocate their output through gralloc; audio components never
/// ask for a GRAPHIC buffer, so they work, and video ones do, so they fail.
/// This is exactly what Waydroid avoids by shipping the switch off.
///
/// Registration is therefore NOT evidence of function, and a report that
/// counts registered decoders is not a report that video works.
pub fn codec2_video_risk(gralloc: &str, ccodec: Option<&str>) -> Option<String> {
    if ccodec != Some("1") { return None; }
    let g = gralloc.trim();
    if g.is_empty() { return None; }
    if !FALLBACK_GRALLOC.iter().any(|f| g.contains(f)) { return None; }
    Some(format!(
        "Codec2 is ON with the fallback gralloc ({g}). Its video components \
         allocate output through gralloc, and on this path that allocation \
         was MEASURED to fail: the decoders register, audio plays, no picture \
         appears and the app crashes. The count of live decoders below says \
         they registered, not that they work."))
}

// ---------------------------------------------------------------------------
// Findings
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Severity {
    /// Nothing to do here.
    Ok,
    /// Works, but on the CPU while the host has a decoder sitting idle.
    /// This is the only actionable state.
    Gap,
    /// Something is actually broken.
    Broken,
    /// On the CPU in the guest, and MEASURED to be unavailable on the host
    /// too. There is no lever here — which is a finding, not an unknown.
    NoLever,
    /// Could not be determined. Stated, never guessed.
    Undetermined,
}

/// What the host can decode in hardware, and how confident we are.
///
/// The distinction matters more than it looks. An earlier version of this
/// report passed an empty capability list when the host had not been probed
/// at all, so every format came back as "the host cannot decode this either".
/// That is a claim, and it was not true — the host decodes all of them.
/// Absence of evidence had been rendered as evidence of absence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum HostCaps {
    /// Verified by actually decoding a clip through the hardware decoder.
    Measured(Vec<String>),
    /// Not probed, or the probe could not run.
    Unknown,
}

impl HostCaps {
    /// `None` when nothing is known — NOT `false`.
    pub fn can_decode(&self, mime: &str) -> Option<bool> {
        match self {
            HostCaps::Measured(m) => Some(m.iter().any(|x| x == mime)),
            HostCaps::Unknown => None,
        }
    }

    pub fn mimes(&self) -> &[String] {
        match self {
            HostCaps::Measured(m) => m,
            HostCaps::Unknown => &[],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub id: String,
    pub title: String,
    pub detail: String,
    pub severity: Severity,
}

/// Where the codec list was read from.
///
/// A live list is ground truth: it is what Android actually registered. The
/// image is only what the image DECLARES, which is not the same thing — a
/// declaration whose backing file is missing produces no codec at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    /// Read from a running Android via `dumpsys`.
    Live,
    /// Read from the image on disk. Android was not running.
    Image,
    /// Neither could be read.
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub source: Source,
    pub guest: Vec<Codec>,
    pub host_profiles: Vec<VaProfile>,
    pub missing_includes: Vec<String>,
    pub findings: Vec<Finding>,
}

/// The video MIME types the guest can decode, and in what.
///
/// Encoders are excluded: a game plays video, it does not record it.
pub fn guest_decode_mimes(codecs: &[Codec]) -> Vec<(String, CodecKind)> {
    let mut out: Vec<(String, CodecKind)> = Vec::new();
    for c in codecs.iter().filter(|c| !c.encoder && c.mime.starts_with("video/")) {
        match out.iter_mut().find(|(m, _)| *m == c.mime) {
            // Hardware wins the entry: if any component for this MIME is
            // hardware, the format is hardware-decodable.
            Some(e) => {
                if c.kind == CodecKind::Hardware { e.1 = CodecKind::Hardware; }
            }
            None => out.push((c.mime.clone(), c.kind)),
        }
    }
    out.sort_by(|a, b| mime_weight(&b.0).cmp(&mime_weight(&a.0)).then(a.0.cmp(&b.0)));
    out
}

/// Builds the whole diagnosis.
///
/// Deliberately makes NO claim about what closing a gap would buy. The gap is a
/// fact and is reported as one; the benefit needs a measurement, and until that
/// measurement exists saying "this would be faster" is a guess.
pub fn diagnose(
    source: Source,
    guest: &[Codec],
    host: &HostCaps,
    missing: &[String],
) -> Vec<Finding> {
    let mut out = Vec::new();

    // 1. A broken include chain is reported first: it silently REMOVES codecs,
    //    so every later finding is read against an incomplete list.
    for m in missing {
        out.push(Finding {
            id: format!("include.{m}"),
            title: format!("media_codecs.xml refers to a missing file: {m}"),
            detail: format!(
                "The image declares <Include href=\"{m}\"/> but that file is not \
                 in /vendor/etc. Whatever codecs it would have declared are \
                 absent, and Android reports no error for this — the codec \
                 simply never appears in the list."),
            severity: Severity::Broken,
        });
    }

    let guest_mimes = guest_decode_mimes(guest);

    if guest_mimes.is_empty() {
        out.push(Finding {
            id: "guest.empty".into(),
            title: "No video decoder could be read for the guest".into(),
            detail: "Neither a live codec list nor the image declarations could \
                     be read, so nothing is claimed about the guest here."
                .into(),
            severity: Severity::Undetermined,
        });
        return out;
    }

    // 2. Per format: what decodes it in the guest, and could the host do better.
    for (mime, kind) in &guest_mimes {
        let label = mime_label(mime);
        if *kind == CodecKind::Hardware {
            out.push(Finding {
                id: format!("codec.{mime}"),
                title: format!("{label}: hardware decode in the guest"),
                detail: "Android has a hardware component for this format.".into(),
                severity: Severity::Ok,
            });
            continue;
        }
        let (severity, detail) = match host.can_decode(mime) {
            Some(true) => (Severity::Gap, format!(
                "Android decodes {label} on the CPU. The host was MEASURED to \
                 decode it in hardware, and nothing carries frames between the \
                 two. What that costs is not measured — run `liw video probe` \
                 before drawing any conclusion.")),
            Some(false) => (Severity::NoLever, format!(
                "Android decodes {label} on the CPU, and the host hardware was \
                 measured NOT to decode this format either. There is nothing to \
                 hand the work to.")),
            None => (Severity::Undetermined, format!(
                "Android decodes {label} on the CPU. Whether the host could do \
                 it in hardware was NOT determined, so no gap is claimed.")),
        };
        out.push(Finding { id: format!("codec.{mime}"), title: match severity {
            Severity::Gap => format!("{label}: CPU in the guest, hardware idle on the host"),
            Severity::NoLever => format!("{label}: CPU on both sides"),
            _ => format!("{label}: CPU in the guest, host not determined"),
        }, detail, severity });
    }

    // 3. Formats the host can decode that the guest cannot play at all.
    for m in host.mimes() {
        if !guest_mimes.iter().any(|(g, _)| g == m) {
            out.push(Finding {
                id: format!("host.only.{m}"),
                title: format!("{}: host can decode it, Android cannot play it",
                               mime_label(m)),
                detail: "The guest declares no component for this format, so \
                         content in it does not play at all."
                    .into(),
                severity: Severity::Undetermined,
            });
        }
    }

    if source == Source::Image {
        out.push(Finding {
            id: "source.image".into(),
            title: "Read from the image, not from a running Android".into(),
            detail: "This is what the image DECLARES. What Android actually \
                     registered can be smaller — a declaration whose file is \
                     missing yields no codec. Start the session and run this \
                     again for the real list."
                .into(),
            severity: Severity::Undetermined,
        });
    }

    out
}

/// Counts findings by severity: (ok, gap, broken, no-lever, undetermined).
pub fn summarise(f: &[Finding]) -> (usize, usize, usize, usize, usize) {
    let c = |s: Severity| f.iter().filter(|x| x.severity == s).count();
    (c(Severity::Ok), c(Severity::Gap), c(Severity::Broken),
     c(Severity::NoLever), c(Severity::Undetermined))
}

// ---------------------------------------------------------------------------
// Declared vs registered
// ---------------------------------------------------------------------------

/// Why the components the image declares never appear at runtime.
///
/// Several reasons can apply at once, so this is a report rather than a single
/// verdict. Collapsing it to one cause would mean picking whichever was
/// checked first and calling it the answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Shortfall {
    /// Codec2 is switched off, so no `c2.*` component registers at all.
    ///
    /// Waydroid sets `debug.stagefright.ccodec=0` when it finds no HAL
    /// gralloc (lxc.py). Codec2's software components allocate graphic
    /// buffers through gralloc, and on the fallback path that allocation
    /// fails — the switch trades newer decoders for working ones.
    pub codec2_disabled: bool,
    /// Components AOSP gates on the device type: (component, domain).
    /// Their absence on an ordinary device is expected.
    pub domain_gated: Vec<(String, String)>,
    /// Absences nothing here accounts for. Named as unexplained, not guessed.
    pub unexplained: Vec<String>,
}

impl Shortfall {
    pub fn is_empty(&self) -> bool {
        !self.codec2_disabled
            && self.domain_gated.is_empty()
            && self.unexplained.is_empty()
    }
}

/// Compares what the image declares against what Android registered.
///
/// The comparison is only worth making with the CAUSE attached. Listing
/// fourteen "declared but not registered" components with no explanation
/// reads as fourteen faults; it was one setting.
pub fn explain_shortfall(
    declared: &[Codec],
    registered: &[String],
    ccodec: Option<&str>,
) -> Shortfall {
    let missing: Vec<&Codec> = declared.iter()
        .filter(|d| !registered.iter().any(|r| r == &d.name))
        .collect();
    let mut s = Shortfall::default();
    if missing.is_empty() { return s; }

    // A gated component is accounted for first: it would be missing whatever
    // the Codec2 switch said, so blaming the switch for it would be wrong.
    let mut rest: Vec<&Codec> = Vec::new();
    for c in missing {
        match &c.domain {
            Some(d) => s.domain_gated.push((c.name.clone(), d.clone())),
            None => rest.push(c),
        }
    }
    if rest.is_empty() { return s; }

    // Every remaining absence being a Codec2 one, with the switch off, is not
    // a coincidence — it is the switch.
    if rest.iter().all(|c| c.name.starts_with("c2.")) && ccodec == Some("0") {
        s.codec2_disabled = true;
        return s;
    }
    s.unexplained = rest.iter().map(|c| c.name.clone()).collect();
    s
}

/// Video formats that have no decoder at runtime at all.
///
/// This is the part that bites a user rather than a benchmark: a format with
/// no component does not play slowly, it does not play.
/// Returns (mime, gated_by_domain) for each format with no live decoder.
///
/// The flag matters: a format whose only component is gated to TV devices is
/// absent by design on a phone, and reporting that as a fault sends the reader
/// after a bug that is not there.
pub fn unplayable(declared: &[Codec], registered: &[String]) -> Vec<(String, bool)> {
    let live: Vec<&Codec> = declared.iter()
        .filter(|c| !c.encoder && registered.iter().any(|r| r == &c.name))
        .collect();
    let mut out: Vec<(String, bool)> = Vec::new();
    for c in declared.iter().filter(|c| !c.encoder && c.mime.starts_with("video/")) {
        if live.iter().any(|l| l.mime == c.mime) { continue; }
        match out.iter_mut().find(|(m, _)| *m == c.mime) {
            // Only gated if EVERY component for the format is gated.
            Some(e) => e.1 = e.1 && c.domain.is_some(),
            None => out.push((c.mime.clone(), c.domain.is_some())),
        }
    }
    out.sort_by(|a, b| mime_weight(&b.0).cmp(&mime_weight(&a.0)).then(a.0.cmp(&b.0)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Codec2's video path ---------------------------------------------

    /// The measured failure: switch on, fallback gralloc, video broken.
    #[test]
    fn codec2_on_a_fallback_gralloc_is_flagged() {
        for g in ["gbm", "default", "minigbm_gbm_mesa"] {
            assert!(codec2_video_risk(g, Some("1")).is_some(), "{g}");
        }
    }

    /// With the switch off there is nothing to warn about — the OMX path is
    /// what Waydroid ships and it works.
    #[test]
    fn the_warning_only_applies_when_codec2_is_on() {
        assert_eq!(codec2_video_risk("gbm", Some("0")), None);
        assert_eq!(codec2_video_risk("gbm", None), None);
    }

    /// A real vendor HAL gralloc is the case Codec2 was designed for. Warning
    /// there would be spreading one machine's result to machines it never
    /// applied to.
    #[test]
    fn a_hal_gralloc_is_not_warned_about() {
        assert_eq!(codec2_video_risk("android", Some("1")), None);
        assert_eq!(codec2_video_risk("qcom", Some("1")), None);
    }

    #[test]
    fn an_unknown_gralloc_claims_nothing() {
        assert_eq!(codec2_video_risk("", Some("1")), None);
        assert_eq!(codec2_video_risk("   ", Some("1")), None);
    }

    /// The wording must not let a reader take registration for function —
    /// that is the mistake the measurement caught.
    #[test]
    fn the_warning_separates_registering_from_working() {
        let w = codec2_video_risk("gbm", Some("1")).unwrap();
        assert!(w.contains("not that they work"), "{w}");
    }

    // --- declared vs registered -------------------------------------------

    fn c2(name: &str, mime: &str) -> Codec {
        Codec { name: name.into(), mime: mime.into(),
                kind: CodecKind::Software, encoder: false, domain: None }
    }

    fn gated(name: &str, mime: &str, domain: &str) -> Codec {
        Codec { name: name.into(), mime: mime.into(),
                kind: CodecKind::Software, encoder: false,
                domain: Some(domain.into()) }
    }

    /// The real case: every c2.* is absent and the switch is off. That is one
    /// finding, not fourteen.
    #[test]
    fn codec2_switch_explains_every_missing_c2_component() {
        let declared = vec![c2("c2.android.avc.decoder", "video/avc"),
                            c2("c2.android.av1.decoder", "video/av01"),
                            c2("OMX.google.h264.decoder", "video/avc")];
        let reg = vec!["OMX.google.h264.decoder".to_string()];
        let s = explain_shortfall(&declared, &reg, Some("0"));
        assert!(s.codec2_disabled);
        assert!(s.unexplained.is_empty(), "the switch accounts for all of them");
    }

    /// With the switch ON, the same absence is NOT explained by it, and
    /// saying it was would be inventing a cause.
    #[test]
    fn the_switch_only_explains_it_when_it_is_actually_off() {
        let declared = vec![c2("c2.android.avc.decoder", "video/avc")];
        let reg: Vec<String> = vec![];
        for prop in [Some("1"), None] {
            let s = explain_shortfall(&declared, &reg, prop);
            assert!(!s.codec2_disabled, "prop {prop:?}");
            assert_eq!(s.unexplained.len(), 1);
        }
    }

    /// A missing OMX component has nothing to do with the Codec2 switch.
    #[test]
    fn a_missing_omx_component_is_not_blamed_on_codec2() {
        let declared = vec![c2("OMX.google.vp9.decoder", "video/x-vnd.on2.vp9")];
        let s = explain_shortfall(&declared, &[], Some("0"));
        assert!(!s.codec2_disabled);
        assert_eq!(s.unexplained.len(), 1);
    }

    #[test]
    fn nothing_missing_is_no_finding() {
        let declared = vec![c2("OMX.google.h264.decoder", "video/avc")];
        let reg = vec!["OMX.google.h264.decoder".to_string()];
        assert!(explain_shortfall(&declared, &reg, Some("0")).is_empty());
    }

    /// Measured on this machine: with Codec2 ON, the ONLY absentees were the
    /// two components carrying a domain attribute. Those are gated by AOSP on
    /// the device type and would be missing whatever the switch said, so the
    /// switch must not be blamed for them.
    #[test]
    fn domain_gated_components_are_not_a_fault() {
        let declared = vec![
            c2("c2.android.avc.decoder", "video/avc"),
            gated("c2.android.mpeg2.decoder", "video/mpeg2", "tv"),
        ];
        let reg = vec!["c2.android.avc.decoder".to_string()];
        let s = explain_shortfall(&declared, &reg, Some("1"));
        assert_eq!(s.domain_gated, vec![("c2.android.mpeg2.decoder".into(),
                                         "tv".into())]);
        assert!(s.unexplained.is_empty(), "a gate is an explanation");
        assert!(!s.codec2_disabled);
    }

    /// A gated component must not be swept into the Codec2 verdict either:
    /// it would be absent with the switch on.
    #[test]
    fn a_gate_is_accounted_for_before_the_switch() {
        let declared = vec![
            c2("c2.android.avc.decoder", "video/avc"),
            gated("c2.android.mpeg2.decoder", "video/mpeg2", "tv"),
        ];
        let s = explain_shortfall(&declared, &[], Some("0"));
        assert!(s.codec2_disabled, "the ungated one is the switch");
        assert_eq!(s.domain_gated.len(), 1, "the gated one is the gate");
    }

    /// A format whose ONLY component fails to register cannot play at all.
    #[test]
    fn a_format_with_no_live_component_is_unplayable() {
        let declared = vec![c2("c2.android.av1.decoder", "video/av01"),
                            c2("c2.android.avc.decoder", "video/avc"),
                            c2("OMX.google.h264.decoder", "video/avc")];
        let reg = vec!["OMX.google.h264.decoder".to_string()];
        let u = unplayable(&declared, &reg);
        assert_eq!(u, vec![("video/av01".to_string(), false)],
                   "H.264 still has a live component");
    }

    /// An unplayable format is flagged as gated only when EVERY component for
    /// it is gated — otherwise a real absence would be excused by design.
    #[test]
    fn a_format_is_only_excused_when_all_its_components_are_gated() {
        let declared = vec![gated("c2.android.mpeg2.decoder", "video/mpeg2", "tv")];
        assert_eq!(unplayable(&declared, &[]), vec![("video/mpeg2".to_string(), true)]);

        let mixed = vec![gated("c2.android.mpeg2.decoder", "video/mpeg2", "tv"),
                         c2("OMX.google.mpeg2.decoder", "video/mpeg2")];
        assert_eq!(unplayable(&mixed, &[]), vec![("video/mpeg2".to_string(), false)],
                   "one ungated component means the absence is not by design");
    }

    #[test]
    fn nothing_is_unplayable_when_everything_registered() {
        let declared = vec![c2("OMX.google.h264.decoder", "video/avc")];
        let reg = vec!["OMX.google.h264.decoder".to_string()];
        assert!(unplayable(&declared, &reg).is_empty());
    }

    /// The domain attribute must actually be read off the XML, or every gate
    /// silently becomes an unexplained fault.
    #[test]
    fn the_domain_attribute_is_parsed() {
        let xml = r#"<MediaCodec name="c2.android.mpeg2.decoder" type="video/mpeg2" domain="tv" />"#;
        let c = parse_codec_xml(xml);
        assert_eq!(c[0].domain.as_deref(), Some("tv"));
    }

    #[test]
    fn a_declaration_without_a_domain_has_none() {
        let xml = r#"<MediaCodec name="c2.android.avc.decoder" type="video/avc" />"#;
        assert_eq!(parse_codec_xml(xml)[0].domain, None);
    }

    // --- component classification -----------------------------------------    // --- component classification -----------------------------------------

    #[test]
    fn aosp_prefixes_are_software() {
        assert_eq!(codec_kind("c2.android.vp9.decoder"), CodecKind::Software);
        assert_eq!(codec_kind("OMX.google.h264.decoder"), CodecKind::Software);
    }

    #[test]
    fn vendor_prefixes_are_hardware() {
        assert_eq!(codec_kind("c2.qti.avc.decoder"), CodecKind::Hardware);
        assert_eq!(codec_kind("OMX.qcom.video.decoder.avc"), CodecKind::Hardware);
        assert_eq!(codec_kind("c2.v4l2.avc.decoder"), CodecKind::Hardware);
    }

    /// `.secure` is a DRM variant and says nothing about where decoding runs.
    #[test]
    fn secure_suffix_does_not_change_the_kind() {
        assert_eq!(codec_kind("c2.android.avc.decoder.secure"), CodecKind::Software);
        assert_eq!(codec_kind("c2.qti.avc.decoder.secure"), CodecKind::Hardware);
    }

    #[test]
    fn unrecognised_names_are_not_guessed() {
        assert_eq!(codec_kind("something.else"), CodecKind::Unknown);
        assert_eq!(codec_kind(""), CodecKind::Unknown);
    }

    // --- include chain -----------------------------------------------------

    #[test]
    fn includes_are_read_in_order() {
        let xml = r#"<MediaCodecs>
            <Include href="media_codecs_google_audio.xml" />
            <Include href="media_codecs_google_video.xml" />
        </MediaCodecs>"#;
        assert_eq!(parse_includes(xml),
                   vec!["media_codecs_google_audio.xml",
                        "media_codecs_google_video.xml"]);
    }

    /// The real file opens with a commented-out DTD that mentions Include.
    /// Reading it would invent declarations that do not exist.
    #[test]
    fn commented_out_declarations_are_ignored() {
        let xml = r#"
        <!--
          <!ELEMENT Include EMPTY>
          <Include href="ghost.xml" />
        -->
        <MediaCodecs>
            <Include href="real.xml" />
        </MediaCodecs>"#;
        assert_eq!(parse_includes(xml), vec!["real.xml"]);
    }

    #[test]
    fn unterminated_comment_swallows_the_rest() {
        let xml = r#"<MediaCodecs>
            <Include href="before.xml" />
            <!-- <Include href="after.xml" />"#;
        assert_eq!(parse_includes(xml), vec!["before.xml"]);
    }

    #[test]
    fn single_quoted_href_is_read() {
        assert_eq!(parse_includes("<Include href='a.xml'/>"), vec!["a.xml"]);
    }

    #[test]
    fn missing_include_is_found() {
        let declared = vec!["a.xml".to_string(), "b.xml".to_string()];
        let present = vec!["a.xml".to_string()];
        assert_eq!(missing_includes(&declared, &present), vec!["b.xml"]);
    }

    #[test]
    fn nothing_missing_when_all_present() {
        let d = vec!["a.xml".to_string()];
        assert!(missing_includes(&d, &d).is_empty());
    }

    // --- codec xml ---------------------------------------------------------

    #[test]
    fn decoders_and_encoders_are_told_apart() {
        let xml = r#"<MediaCodecs>
          <Decoders>
            <MediaCodec name="OMX.google.vp9.decoder" type="video/x-vnd.on2.vp9" />
          </Decoders>
          <Encoders>
            <MediaCodec name="OMX.google.h264.encoder" type="video/avc" />
          </Encoders>
        </MediaCodecs>"#;
        let c = parse_codec_xml(xml);
        assert_eq!(c.len(), 2);
        assert!(!c[0].encoder, "decoder must not be marked as encoder");
        assert!(c[1].encoder, "encoder must be marked");
    }

    /// media_codecs_performance.xml amends existing entries with update="true".
    /// Treating those as declarations invents codecs that do not exist.
    #[test]
    fn update_entries_are_not_declarations() {
        let xml = r#"<MediaCodec name="c2.android.avc.decoder" type="video/avc" update="true">
                     </MediaCodec>"#;
        assert!(parse_codec_xml(xml).is_empty());
    }

    /// "type" must not be matched inside "subtype".
    #[test]
    fn attribute_lookup_respects_word_start() {
        let tag = r#"<MediaCodec subtype="wrong" name="a" type="video/avc" />"#;
        assert_eq!(attr(tag, "type").as_deref(), Some("video/avc"));
    }

    #[test]
    fn missing_attribute_returns_none() {
        assert_eq!(attr(r#"<MediaCodec name="a" />"#, "type"), None);
    }

    // --- vainfo ------------------------------------------------------------

    const VAINFO: &str = "\
vainfo: VA-API version: 1.22 (libva 2.24.1)
vainfo: Driver version: VA-API NVDEC driver [direct backend]
vainfo: Supported profile and entrypoints
      VAProfileH264Main               : VAEntrypointVLD
      VAProfileH264High               : VAEntrypointVLD
      VAProfileVP9Profile0            : VAEntrypointVLD
      VAProfileHEVCMain               : VAEntrypointVLD
      VAProfileNone                   : VAEntrypointVideoProc
";

    #[test]
    fn vainfo_profiles_are_read() {
        let p = parse_vainfo(VAINFO);
        assert_eq!(p.len(), 5);
        assert!(p.iter().any(|x| x.profile == "VAProfileVP9Profile0"));
    }

    /// The same profile appears on several lines, one per entrypoint. They must
    /// merge: overwriting loses the decode entrypoint when encode comes last.
    #[test]
    fn repeated_profile_lines_merge_entrypoints() {
        let out = "      VAProfileH264Main : VAEntrypointVLD
      VAProfileH264Main : VAEntrypointEncSlice";
        let p = parse_vainfo(out);
        assert_eq!(p.len(), 1, "one profile, not two");
        assert_eq!(p[0].entrypoints.len(), 2);
        assert!(p[0].decodes(), "the VLD entrypoint must survive the merge");
    }

    #[test]
    fn encode_only_profile_does_not_count_as_decode() {
        let p = parse_vainfo("      VAProfileH264Main : VAEntrypointEncSlice");
        assert!(!p[0].decodes());
    }

    #[test]
    fn host_decodable_maps_profiles_to_mimes() {
        let m = host_decodable(&parse_vainfo(VAINFO));
        assert!(m.contains(&"video/avc"));
        assert!(m.contains(&"video/x-vnd.on2.vp9"));
        assert!(m.contains(&"video/hevc"));
    }

    /// H264Main and H264High both map to video/avc; it must appear once.
    #[test]
    fn duplicate_mimes_are_collapsed() {
        let m = host_decodable(&parse_vainfo(VAINFO));
        assert_eq!(m.iter().filter(|x| **x == "video/avc").count(), 1);
    }

    #[test]
    fn video_proc_is_not_a_codec() {
        assert_eq!(va_profile_mime("VAProfileNone"), None);
    }

    // --- diagnosis ---------------------------------------------------------

    fn sw(name: &str, mime: &str) -> Codec {
        Codec { name: name.into(), mime: mime.into(),
                kind: CodecKind::Software, encoder: false, domain: None }
    }

    /// The host capability, measured.
    fn measured(mimes: &[&str]) -> HostCaps {
        HostCaps::Measured(mimes.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn software_guest_plus_hardware_host_is_a_gap() {
        let guest = vec![sw("c2.android.avc.decoder", "video/avc")];
        let f = diagnose(Source::Live, &guest, &measured(&["video/avc"]), &[]);
        let avc = f.iter().find(|x| x.id == "codec.video/avc").unwrap();
        assert_eq!(avc.severity, Severity::Gap);
    }

    /// THE regression guard for this module.
    ///
    /// An unprobed host used to arrive as an empty capability list, which read
    /// as "the host cannot decode this" and put a false negative in front of
    /// the user for every single format. Not knowing must never render as no.
    #[test]
    fn unprobed_host_is_never_reported_as_incapable() {
        let guest = vec![sw("c2.android.avc.decoder", "video/avc")];
        let f = diagnose(Source::Live, &guest, &HostCaps::Unknown, &[]);
        let avc = f.iter().find(|x| x.id == "codec.video/avc").unwrap();
        assert_eq!(avc.severity, Severity::Undetermined,
                   "an unprobed host must not be reported as having no decoder");
        assert!(!avc.detail.contains("nothing to hand"),
                "must not assert absence: {}", avc.detail);
    }

    /// And the opposite: a host that WAS probed and genuinely lacks the format
    /// is a definite negative, not an unknown.
    #[test]
    fn measured_absence_is_stated_as_absence() {
        let guest = vec![sw("c2.android.av1.decoder", "video/av01")];
        let f = diagnose(Source::Live, &guest, &measured(&["video/avc"]), &[]);
        let av1 = f.iter().find(|x| x.id == "codec.video/av01").unwrap();
        assert_eq!(av1.severity, Severity::NoLever);
    }

    #[test]
    fn host_caps_distinguishes_no_from_unknown() {
        assert_eq!(HostCaps::Unknown.can_decode("video/avc"), None);
        assert_eq!(measured(&[]).can_decode("video/avc"), Some(false));
        assert_eq!(measured(&["video/avc"]).can_decode("video/avc"), Some(true));
    }

    /// If the host cannot decode it either, there is no gap to close — calling
    /// it one would invent a fix that does not exist.
    #[test]
    fn no_host_decoder_means_no_gap() {
        let guest = vec![sw("c2.android.av1.decoder", "video/av01")];
        let f = diagnose(Source::Live, &guest, &measured(&["video/avc"]), &[]);
        let av1 = f.iter().find(|x| x.id == "codec.video/av01").unwrap();
        assert_ne!(av1.severity, Severity::Gap, "no host decoder, so no lever");
    }

    #[test]
    fn hardware_guest_codec_is_ok() {
        let guest = vec![Codec { name: "c2.qti.avc.decoder".into(),
                                 mime: "video/avc".into(),
                                 kind: CodecKind::Hardware, encoder: false,
                                 domain: None }];
        let f = diagnose(Source::Live, &guest, &measured(&["video/avc"]), &[]);
        assert_eq!(f[0].severity, Severity::Ok);
    }

    /// A missing include silently removes codecs, so it must be reported
    /// BEFORE the codec list — the list is read against it.
    #[test]
    fn missing_include_is_reported_first() {
        let guest = vec![sw("c2.android.avc.decoder", "video/avc")];
        let miss = vec!["media_codecs_ffmpeg.xml".to_string()];
        let f = diagnose(Source::Live, &guest, &measured(&["video/avc"]), &miss);
        assert_eq!(f[0].severity, Severity::Broken);
        assert!(f[0].title.contains("media_codecs_ffmpeg.xml"));
    }

    #[test]
    fn encoders_are_not_diagnosed() {
        let guest = vec![Codec { name: "OMX.google.h264.encoder".into(),
                                 mime: "video/avc".into(),
                                 kind: CodecKind::Software, encoder: true,
                                 domain: None }];
        assert!(guest_decode_mimes(&guest).is_empty(),
                "a game plays video, it does not record it");
    }

    /// If ANY component for a format is hardware, the format is hardware.
    #[test]
    fn hardware_wins_over_software_for_the_same_mime() {
        let guest = vec![
            sw("c2.android.avc.decoder", "video/avc"),
            Codec { name: "c2.qti.avc.decoder".into(), mime: "video/avc".into(),
                    kind: CodecKind::Hardware, encoder: false, domain: None },
        ];
        let m = guest_decode_mimes(&guest);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].1, CodecKind::Hardware);
    }

    #[test]
    fn the_common_formats_are_reported_first() {
        let guest = vec![
            sw("c2.android.mpeg4.decoder", "video/mp4v-es"),
            sw("c2.android.avc.decoder", "video/avc"),
        ];
        let m = guest_decode_mimes(&guest);
        assert_eq!(m[0].0, "video/avc", "H.264 carries most real video");
    }

    #[test]
    fn empty_guest_list_claims_nothing() {
        let f = diagnose(Source::None, &[], &HostCaps::Unknown, &[]);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].severity, Severity::Undetermined);
    }

    /// Reading the image is weaker evidence than reading a running Android,
    /// and the report has to say so.
    #[test]
    fn image_source_is_flagged_as_weaker_evidence() {
        let guest = vec![sw("c2.android.avc.decoder", "video/avc")];
        let f = diagnose(Source::Image, &guest, &HostCaps::Unknown, &[]);
        assert!(f.iter().any(|x| x.id == "source.image"));
    }

    #[test]
    fn live_source_adds_no_caveat() {
        let guest = vec![sw("c2.android.avc.decoder", "video/avc")];
        let f = diagnose(Source::Live, &guest, &HostCaps::Unknown, &[]);
        assert!(!f.iter().any(|x| x.id == "source.image"));
    }

    #[test]
    fn summarise_counts_each_severity() {
        let guest = vec![sw("c2.android.avc.decoder", "video/avc")];
        let miss = vec!["x.xml".to_string()];
        let f = diagnose(Source::Live, &guest, &measured(&["video/avc"]), &miss);
        let (_, gap, broken, _, _) = summarise(&f);
        assert_eq!(broken, 1);
        assert_eq!(gap, 1);
    }
}
