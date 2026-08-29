//! `liw art` — pick per-game artwork.
//!
//! The APK is a source of CANDIDATES, never an answer. Measured on three real
//! apps, "largest image" picks a volume glyph for one game and an ad SDK
//! banner for another (see `liw_core::art`). A person looks at the shortlist
//! and says which one is the game.

use anyhow::{Context, Result};
use liw_core::art;
use std::path::PathBuf;

pub fn list(package: &str) -> Result<()> {
    let apks = art::apks_for(package);
    if apks.is_empty() {
        println!("No APK found for '{package}'.");
        println!("Waydroid keeps them under ~/.local/share/waydroid/data/app —");
        println!("a system app that ships with the image has none there.");
        return Ok(());
    }
    println!("APKs   : {}", apks.len());
    if let Some(cur) = art::art_for(package) {
        println!("Current: {}", cur.display());
    }

    let cands = art::candidates(package);
    if cands.is_empty() {
        println!();
        println!("No plausible artwork inside. Point at your own image instead:");
        println!("  liw art use {package} /path/to/picture.png");
        return Ok(());
    }
    println!();
    println!("Candidates — LOOK before choosing; APKs are full of ad assets:");
    for (i, c) in cands.iter().enumerate().take(12) {
        println!("  [{i}] {:>5}x{:<5} {:>7} KB  {}",
            c.width, c.height, c.bytes / 1024, c.entry);
    }
    println!();
    println!("Preview them all, then pick one:");
    println!("  liw art dump {package} /tmp/{package}-art");
    println!("  liw art use  {package} 0");
    Ok(())
}

/// Extracts every candidate so they can be looked at.
pub fn dump(package: &str, dir: PathBuf) -> Result<()> {
    let cands = art::candidates(package);
    if cands.is_empty() {
        println!("Nothing to dump.");
        return Ok(());
    }
    std::fs::create_dir_all(&dir)?;
    for (i, c) in cands.iter().enumerate().take(12) {
        let ext = if c.entry.to_lowercase().ends_with(".webp") { "webp" } else { "png" };
        let dest = dir.join(format!("{i}-{}x{}.{ext}", c.width, c.height));
        art::extract(c, &dest).with_context(|| format!("could not extract {}", c.entry))?;
        println!("  [{i}] {}", dest.display());
    }
    println!();
    println!("Open them, then: liw art use {package} <n>");
    Ok(())
}

/// `what` is either a candidate index or a path to an image.
pub fn use_art(package: &str, what: &str) -> Result<()> {
    let src: PathBuf = match what.parse::<usize>() {
        Ok(n) => {
            let cands = art::candidates(package);
            let c = cands.get(n)
                .with_context(|| format!("no candidate [{n}] — `liw art list {package}`"))?;
            // The extension comes from the ENTRY, not from the package name:
            // a package is full of dots and `Path::extension` would take its
            // last segment.
            let ext = if c.entry.to_lowercase().ends_with(".webp") { "webp" } else { "png" };
            let tmp = std::env::temp_dir().join(format!("liw-art-{package}.{ext}"));
            art::extract(c, &tmp).context("could not extract the candidate")?;
            tmp
        }
        Err(_) => PathBuf::from(what),
    };
    if !src.is_file() {
        anyhow::bail!("no such file: {}", src.display());
    }
    let dest = art::set_art(package, &src).context("could not install the artwork")?;
    println!("Artwork set: {}", dest.display());
    println!("liw-ui picks it up on its next start.");
    Ok(())
}

pub fn clear(package: &str) -> Result<()> {
    art::clear_art(package)?;
    println!("Artwork cleared for {package}.");
    Ok(())
}
