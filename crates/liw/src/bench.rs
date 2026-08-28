//! `liw bench` — frame timing and resource measurement.

use anyhow::{Context, Result};
use liw_core::bench::{parse_latency, sample_interval_ms, FrameData};
use liw_core::hostsample::{self, CpuMeter, HostSample};
use liw_core::HelperClient;

/// Probes a layer candidate: does it actually return frame data?
///
/// Choosing by name picks the wrong layer — `ActivityRecord{...}` is a window
/// record, not a buffer layer, and produces no `--latency` output.
/// This actually happened: selecting by name captured 0 frames.
pub(crate) async fn probe(h: &HelperClient, layer: &str) -> usize {
    match h.surface_latency(layer).await {
        Ok(raw) => parse_latency(&raw).map(|s| s.presents.len()).unwrap_or(0),
        Err(_) => 0,
    }
}

pub(crate) async fn pick_layer(h: &HelperClient, pkg: &str) -> Result<String> {
    let list = h.surface_layers().await.context("could not get the layer list")?;
    let mut cands: Vec<&str> = list.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && l.to_lowercase().contains(&pkg.to_lowercase()))
        .filter(|l| !l.contains("ActivityRecord") && !l.contains("Dim")
                 && !l.contains("Blur") && !l.contains("Splash"))
        .collect();
    // BLAST/SurfaceView layers are a game's presentation surface; prefer them.
    cands.sort_by_key(|l| !(l.contains("BLAST") || l.contains("SurfaceView")));

    for c in &cands {
        let n = probe(h, c).await;
        println!("  aday: {:<60} -> {n} kare", &c[..c.len().min(60)]);
        if n > 5 { return Ok((*c).to_string()); }
    }
    anyhow::bail!(
        "no layer returns frame data for '{pkg}'.\n\
         Is the game in the foreground and MOVING? A static screen produces no frames.")
}

pub async fn run(pkg: String, duration_s: u64) -> Result<()> {
    let h = HelperClient::connect().await
        .context("could not connect to liwd-helper — systemctl status liwd-helper")?;

    println!("Looking for a layer...");
    let layer = pick_layer(&h, &pkg).await?;
    println!("Katman : {layer}");

    // The sampling interval is DERIVED from the refresh rate: a fixed interval
    // overflows the 128-frame buffer at high refresh rates and loses frames.
    let first = parse_latency(&h.surface_latency(&layer).await?)
        .context("could not parse the first snapshot")?;
    let interval = sample_interval_ms(first.refresh_ns);
    println!("Refresh: {:.2} ms ({:.1} Hz)  ->  sampling every {interval} ms",
        first.refresh_ns as f64 / 1e6, 1e9 / first.refresh_ns as f64);
    println!("Length : {duration_s}s — ACTUALLY PLAY (a static screen ruins the measurement)");
    println!();

    let mut fd = FrameData::new();
    let mut host: Vec<HostSample> = Vec::new();
    let mut cpu = CpuMeter::default();
    let mut host_tick = tokio::time::interval(std::time::Duration::from_secs(1));
    let mut frame_tick = tokio::time::interval(std::time::Duration::from_millis(interval));
    frame_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(duration_s);
    let mut snaps = 0u32;

    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            _ = frame_tick.tick() => {
                if let Ok(raw) = h.surface_latency(&layer).await {
                    if let Some(s) = parse_latency(&raw) { fd.add(&s); snaps += 1; }
                }
            }
            _ = host_tick.tick() => host.push(hostsample::sample(&mut cpu).await),
            _ = tokio::signal::ctrl_c() => break,
        }
    }

    println!("{snaps} snapshots taken");
    println!();
    println!("{}", "=".repeat(58));
    if fd.interval_count() < 30 {
        println!("Not enough data ({} intervals).", fd.interval_count());
        println!("Was the game in the foreground and moving?");
    } else {
        println!("FRAME TIMING   ({} intervals, {} unique frames)",
            fd.interval_count(), fd.frame_count());
        println!("  p50            : {:7.2} ms   -> {:6.1} FPS",
            fd.percentile(50.0), 1000.0 / fd.percentile(50.0).max(0.001));
        println!("  p95            : {:7.2} ms", fd.percentile(95.0));
        println!("  p99            : {:7.2} ms", fd.percentile(99.0));
        println!("  p99.9          : {:7.2} ms", fd.percentile(99.9));
        println!("  worst          : {:7.2} ms", fd.percentile(100.0));
        println!("  ortalama       : {:7.2} ms   -> {:6.1} FPS",
            fd.mean_ms(), 1000.0 / fd.mean_ms().max(0.001));
        println!("  refresh        : {:7.2} ms   -> {:6.1} Hz",
            fd.refresh_ms(), 1000.0 / fd.refresh_ms().max(0.001));
        if fd.is_below_refresh() {
            println!("  NOTE: the game is locked to {:.0} FPS, the display is {:.0} Hz.",
                fd.target_fps(), 1000.0 / fd.refresh_ms().max(0.001));
            println!("       Jank is measured against the GAME period ({:.2} ms), not the display.",
                fd.target_period_ms());
        }
        println!("  jank >1.5x     : {:5}  (%{:.2})", fd.jank_count(1.5), fd.jank_pct(1.5));
        println!("  jank >2x       : {:5}  (%{:.2})", fd.jank_count(2.0), fd.jank_pct(2.0));
        let cov = fd.coverage_pct();
        println!("  capture coverage: {cov:.0}%");
        if cov < 60.0 {
            println!("  WARNING: low coverage — the numbers may not be representative");
        }
    }

    if !host.is_empty() {
        println!();
        println!("HOST RESOURCE USE ({} samples)", host.len());
        for (label, vals, unit) in [
            ("GPU", host.iter().map(|h| h.gpu_pct).collect::<Vec<_>>(), "%"),
            ("VRAM", host.iter().map(|h| h.vram_mb).collect(), "MB"),
            ("CPU (sistem)", host.iter().map(|h| h.cpu_pct).collect(), "%"),
            ("RAM", host.iter().map(|h| h.ram_used_mb).collect(), "MB"),
            ("mem.pressure", host.iter().map(|h| h.mem_pressure).collect(), ""),
        ] {
            let (mean, peak) = hostsample::summarise(&vals);
            println!("  {label:<14}: ort {mean:8.1}{unit}   tepe {peak:8.1}{unit}");
        }
    }
    println!("{}", "=".repeat(58));
    Ok(())
}
