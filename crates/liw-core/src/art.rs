//! Per-game artwork.
//!
//! # Why this is not automatic
//!
//! A store front looks rich because it has key art. Waydroid caches icons at
//! 54x54 and nothing else, so the obvious idea is to pull artwork out of the
//! installed APK. That was measured on three installed apps and it does not
//! work:
//!
//! * Special Forces Group 2 — the largest landscape image IS the official
//!   1280x720 key art. But the largest square image, the obvious icon
//!   candidate, is a **speaker glyph** from the volume UI.
//! * Subway Surfers — the largest landscape image is a **Mintegral ad SDK**
//!   banner, and the best square candidate is cross-promo art for a
//!   completely different game.
//! * Instagram — the largest landscape image is a filter atlas.
//!
//! No size or aspect rule separates key art from ad assets, and guessing
//! wrong puts a speaker icon or someone else's advertisement on the user's
//! game. GameLoop has real art because Tencent curates a catalogue, which is
//! a content operation and not a trick we can copy.
//!
//! So: artwork is a FILE THE USER PLACES, and the APK is offered only as a
//! source of candidates for a human to pick from.

use std::path::{Path, PathBuf};

/// Where user-supplied artwork lives: `<data>/liwinux/art/<package>.<ext>`.
pub fn art_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(base.join("liwinux/art"))
}

/// Artwork for a package, if the user has set any.
pub fn art_for(package: &str) -> Option<PathBuf> {
    let dir = art_dir()?;
    ["png", "jpg", "jpeg", "webp"].iter()
        .map(|e| dir.join(format!("{package}.{e}")))
        .find(|p| p.is_file())
}

/// An image inside an APK that might be worth using.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Which APK it came from (a package can ship several splits).
    pub apk: PathBuf,
    /// Entry name inside the archive.
    pub entry: String,
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
}

impl Candidate {
    pub fn aspect(&self) -> f32 {
        if self.height == 0 { return 0.0 }
        self.width as f32 / self.height as f32
    }
}

/// PNG dimensions from the first bytes of the file.
///
/// Header-only on purpose: an APK holds hundreds of images and decoding them
/// all to ask how big they are would take seconds.
pub fn png_size(head: &[u8]) -> Option<(u32, u32)> {
    if head.len() < 24 || &head[..8] != b"\x89PNG\r\n\x1a\n" || &head[12..16] != b"IHDR" {
        return None;
    }
    Some((
        u32::from_be_bytes(head[16..20].try_into().ok()?),
        u32::from_be_bytes(head[20..24].try_into().ok()?),
    ))
}

/// WebP dimensions. Three container variants, all of them common in APKs.
pub fn webp_size(head: &[u8]) -> Option<(u32, u32)> {
    if head.len() < 30 || &head[..4] != b"RIFF" || &head[8..12] != b"WEBP" {
        return None;
    }
    match &head[12..16] {
        b"VP8X" => {
            let w = u32::from_le_bytes([head[24], head[25], head[26], 0]) + 1;
            let h = u32::from_le_bytes([head[27], head[28], head[29], 0]) + 1;
            Some((w, h))
        }
        b"VP8L" => {
            let n = u32::from_le_bytes(head[21..25].try_into().ok()?);
            Some(((n & 0x3FFF) + 1, ((n >> 14) & 0x3FFF) + 1))
        }
        b"VP8 " => Some((
            u16::from_le_bytes([head[26], head[27]]) as u32 & 0x3FFF,
            u16::from_le_bytes([head[28], head[29]]) as u32 & 0x3FFF,
        )),
        _ => None,
    }
}

pub fn image_size(head: &[u8]) -> Option<(u32, u32)> {
    png_size(head).or_else(|| webp_size(head))
}

/// Is this worth showing a person as possible key art?
///
/// Wide and reasonably large. This is a FILTER for a picker, not a decision:
/// it narrows hundreds of images down to a screenful, and a human says which
/// one is the game.
pub fn plausible(width: u32, height: u32, bytes: u64) -> bool {
    if width < 640 || bytes < 20_000 { return false }
    let aspect = width as f32 / height.max(1) as f32;
    // Landscape, but not a strip: banners and sprite sheets are extremely
    // wide and never look like key art.
    (1.2..=2.6).contains(&aspect)
}

/// Where Waydroid keeps installed APKs.
fn app_root() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(base.join("waydroid/data/app"))
}

/// Every APK belonging to a package (a base plus its splits).
pub fn apks_for(package: &str) -> Vec<PathBuf> {
    let Some(root) = app_root() else { return Vec::new() };
    let mut out = Vec::new();
    // Layout is `<root>/<random>/<package>-<random>/*.apk`.
    let Ok(outer) = std::fs::read_dir(&root) else { return out };
    for a in outer.flatten() {
        let Ok(inner) = std::fs::read_dir(a.path()) else { continue };
        for b in inner.flatten() {
            let name = b.file_name().to_string_lossy().to_string();
            // The directory is `<package>-<base64>`; match on the package
            // part so `com.x` does not also match `com.x.y`.
            if name.split('-').next() != Some(package) { continue }
            let Ok(files) = std::fs::read_dir(b.path()) else { continue };
            for f in files.flatten() {
                let p = f.path();
                if p.extension().is_some_and(|e| e == "apk") { out.push(p); }
            }
        }
    }
    out.sort();
    out
}

/// Images inside a package's APKs that could be key art.
///
/// A shortlist for a human, never an answer. See the module note for what
/// happens when a machine decides this on its own.
pub fn candidates(package: &str) -> Vec<Candidate> {
    let mut out = Vec::new();
    for apk in apks_for(package) {
        let Ok(file) = std::fs::File::open(&apk) else { continue };
        let Ok(mut zip) = zip::ZipArchive::new(file) else { continue };
        for i in 0..zip.len() {
            let (name, bytes) = {
                let Ok(e) = zip.by_index(i) else { continue };
                (e.name().to_string(), e.size())
            };
            let lower = name.to_lowercase();
            if !(lower.ends_with(".png") || lower.ends_with(".webp")) { continue }
            if bytes < 20_000 { continue }
            let mut head = [0u8; 32];
            {
                let Ok(mut e) = zip.by_index(i) else { continue };
                use std::io::Read as _;
                if e.read(&mut head).is_err() { continue }
            }
            let Some((w, h)) = image_size(&head) else { continue };
            if !plausible(w, h, bytes) { continue }
            out.push(Candidate { apk: apk.clone(), entry: name, width: w, height: h, bytes });
        }
    }
    // Biggest first: key art is usually the largest thing that passed.
    out.sort_by_key(|c| std::cmp::Reverse(c.width as u64 * c.height as u64));
    out
}

/// Copies one candidate out of its APK to `dest`.
pub fn extract(c: &Candidate, dest: &Path) -> std::io::Result<()> {
    let file = std::fs::File::open(&c.apk)?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let mut entry = zip.by_name(&c.entry)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    if let Some(d) = dest.parent() { std::fs::create_dir_all(d)?; }
    let mut out = std::fs::File::create(dest)?;
    std::io::copy(&mut entry, &mut out)?;
    Ok(())
}

/// Installs a file as a package's artwork. Returns where it landed.
pub fn set_art(package: &str, src: &Path) -> std::io::Result<PathBuf> {
    let dir = art_dir().ok_or_else(|| std::io::Error::other("no data directory"))?;
    std::fs::create_dir_all(&dir)?;
    // Validated against the known list rather than trusted.
    //
    // `Path::extension` on a temp file named after a package returns the last
    // dotted segment — `set_art(.., "liw-art-com.ForgeGames.Foo")` wrote
    // `<pkg>.foo`, which then matched nothing when reading back. A wrong
    // extension here is silent: the file is written and never found again.
    let ext = src.extension().and_then(|e| e.to_str())
        .map(str::to_lowercase)
        .filter(|e| matches!(e.as_str(), "png" | "jpg" | "jpeg" | "webp"))
        .unwrap_or_else(|| "png".into());
    // One artwork per package: drop any other extension so a leftover .png
    // does not keep winning over a new .jpg.
    for e in ["png", "jpg", "jpeg", "webp"] {
        let _ = std::fs::remove_file(dir.join(format!("{package}.{e}")));
    }
    let dest = dir.join(format!("{package}.{ext}"));
    std::fs::copy(src, &dest)?;
    Ok(dest)
}

pub fn clear_art(package: &str) -> std::io::Result<()> {
    let Some(dir) = art_dir() else { return Ok(()) };
    for e in ["png", "jpg", "jpeg", "webp"] {
        let _ = std::fs::remove_file(dir.join(format!("{package}.{e}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_head(w: u32, h: u32) -> Vec<u8> {
        let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
        v.extend([0, 0, 0, 13]);
        v.extend(b"IHDR");
        v.extend(w.to_be_bytes());
        v.extend(h.to_be_bytes());
        v
    }

    #[test]
    fn png_dimensions_come_from_the_header() {
        assert_eq!(png_size(&png_head(1280, 720)), Some((1280, 720)));
        assert_eq!(png_size(b"not a png"), None);
        assert_eq!(png_size(&[]), None);
    }

    #[test]
    fn webp_lossy_dimensions_parse() {
        // 16 header bytes + enough payload to reach the size fields at 26..30.
        let mut v = b"RIFF\0\0\0\0WEBPVP8 ".to_vec();
        v.extend([0u8; 16]);
        v[26] = 0x00; v[27] = 0x02;   // 512
        v[28] = 0x00; v[29] = 0x01;   // 256
        assert_eq!(webp_size(&v), Some((512, 256)));
    }

    /// The filter has to reject what was actually measured in real APKs, or
    /// the picker hands the user a speaker glyph and an advert.
    #[test]
    fn the_filter_rejects_what_real_apks_are_full_of() {
        // Special Forces Group 2's real key art.
        assert!(plausible(1280, 720, 1_426_070));
        // The "icon candidate" that turned out to be a volume glyph.
        assert!(!plausible(365, 364, 5_155));
        // Subway Surfers' widest image: a Mintegral ad banner.
        assert!(!plausible(2566, 238, 200_000), "ad strips are not key art");
        // Instagram's biggest: a filter atlas.
        assert!(!plausible(3118, 864, 900_000), "atlases are not key art");
        // Small images never qualify however square.
        assert!(!plausible(512, 512, 400_000));
    }

    #[test]
    fn aspect_is_safe_on_degenerate_input() {
        let c = Candidate { apk: PathBuf::new(), entry: String::new(),
                            width: 100, height: 0, bytes: 0 };
        assert_eq!(c.aspect(), 0.0);
    }

    /// Missing artwork must be None, not a path that does not exist — a UI
    /// binding to it would draw a broken image and blame itself.
    #[test]
    fn art_for_an_unknown_package_is_none() {
        assert!(art_for("com.definitely.not.installed.xyz").is_none());
    }

    /// A package name's last dotted segment must never become the file
    /// extension. This shipped once: artwork landed as
    /// `com.ForgeGames.SpecialForcesGroup2.specialforcesgroup2` and was then
    /// invisible to `art_for`, which only looks for image extensions.
    #[test]
    fn a_package_shaped_source_name_does_not_become_an_extension() {
        let dir = std::env::temp_dir().join(format!("liw-art-t{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("liw-art-com.ForgeGames.SpecialForcesGroup2");
        std::fs::write(&src, b"x").unwrap();

        unsafe { std::env::set_var("XDG_DATA_HOME", &dir) };
        let out = set_art("com.ForgeGames.SpecialForcesGroup2", &src).unwrap();
        assert_eq!(out.extension().unwrap(), "png", "got {out:?}");
        assert!(art_for("com.ForgeGames.SpecialForcesGroup2").is_some(),
                "written artwork must be findable again");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
