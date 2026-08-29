//! liw — the liwinux command line client.

mod bench;
mod perf;
mod trace;
mod editor;
mod keymap;
mod profile;
mod video;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use liw_core::{Health, Supervisor, SupervisorConfig};
use zbus::Connection;

const BUS_NAME: &str = "id.liwinux.Manager1";
const OBJ_PATH: &str = "/id/liwinux/Manager1";

#[derive(Parser)]
#[command(name = "liw", version, about = "liwinux — an Android gaming layer for Linux")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Session management
    Session {
        #[command(subcommand)]
        action: SessionAction,
    },
    /// Performance measurement: frame timing and resource use
    Bench {
        /// Android package name
        package: String,
        /// Measurement length (seconds)
        #[arg(short, long, default_value_t = 60)]
        duration: u64,
    },
    /// Find WHY it stutters: frames + Android log + host, on one clock
    Trace {
        /// Android package name
        package: String,
        /// Trace length (seconds)
        #[arg(short, long, default_value_t = 90)]
        duration: u64,
        /// Jank threshold (ms). Derived from the measured cadence if omitted.
        #[arg(long)]
        jank_ms: Option<f64>,
    },
    /// Diagnose performance levers (changes nothing)
    Perf {
        #[command(subcommand)]
        action: PerfAction,
    },
    /// Profile management
    Profile {
        #[command(subcommand)]
        action: ProfileAction,
    },
    /// Video decode: what decodes it, where, and what it costs
    Video {
        #[command(subcommand)]
        action: VideoAction,
    },
    /// Key mapping
    Keymap {
        #[command(subcommand)]
        action: KeymapAction,
    },
}

#[derive(Subcommand)]
enum ProfileAction {
    /// List every profile found
    List,
    /// Show the details of a profile
    Show {
        /// Android package name
        package: String,
    },
    /// Which profile applies to the foreground app
    Which,
    /// Open the VISUAL editor (drag and drop over a screenshot)
    Edit {
        /// Android package name
        package: String,
        /// Sunucu portu (0 = rastgele)
        #[arg(long, default_value_t = 8731)]
        port: u16,
    },
    /// Change a binding's coordinate
    Set {
        /// Android package name
        package: String,
        /// Binding name (see liw profile show)
        binding: String,
        /// X (0..1)
        x: f64,
        /// Y (0..1)
        y: f64,
        /// Field to change: at | center | origin | from | to
        #[arg(long, default_value = "at")]
        field: String,
    },
    /// Poke a binding's coordinate — verify placement visually
    Poke {
        package: String,
        binding: String,
        /// Wait before touching (to bring the target window forward)
        #[arg(long, default_value_t = 5)]
        delay: u64,
    },
    /// Copy the profiles shipped with the repository into the user directory
    Install {
        /// Overwrite existing ones
        #[arg(short, long)]
        force: bool,
        /// Source profile directory (required for an installed binary)
        #[arg(long)]
        from: Option<std::path::PathBuf>,
    },
}

#[derive(Subcommand)]
enum KeymapAction {
    /// List the usable input devices
    Devices,
    /// Start the keymapper inside liwd (survives closing the terminal)
    Start {
        /// Grab the device while a profile is active
        #[arg(short, long)]
        grab: bool,
    },
    /// Stop the keymapper inside liwd
    Stop,
    /// State of the keymapper inside liwd
    Status,
    /// Run the keymapper IN THE FOREGROUND (debugging; ends with Ctrl+C)
    Run {
        /// Grab the device while a profile is active (keys do not reach the desktop)
        #[arg(short, long)]
        grab: bool,
        /// Foreground polling interval (ms)
        #[arg(long, default_value_t = 1000)]
        poll: u64,
    },
    /// Toggle the Android touch indicator (a visual aid for calibration)
    Overlay {
        /// kapat
        #[arg(long)]
        off: bool,
    },
    /// Coordinate sweep: measure which points reach the window
    Sweep {
        /// Axis: x or y
        #[arg(default_value = "x")]
        axis: char,
        /// Number of points
        #[arg(long, default_value_t = 11)]
        count: u32,
        /// Delay between points (ms)
        #[arg(long, default_value_t = 900)]
        gap: u64,
    },
    /// Identify the keyboard by calibration: press a key
    Detect {
        /// Save the detected device to the configuration
        #[arg(short, long)]
        save: bool,
        /// Calibrate the MOUSE instead of the keyboard (by moving it)
        #[arg(short, long)]
        mouse: bool,
        /// Identify the game-mode HOTKEY
        #[arg(long)]
        hotkey: bool,
    },
    /// Watch which device produces which key code (diagnostics)
    Watch {
        /// Watch a single device (default: every keyboard)
        #[arg(short, long)]
        device: Option<std::path::PathBuf>,
    },
    /// Send one touch/drag — injection test independent of the mapping
    Poke {
        /// X (0..1)
        #[arg(default_value_t = 0.5)]
        x: f32,
        /// Y (0..1)
        #[arg(default_value_t = 0.5)]
        y: f32,
        /// Hold / drag duration (ms)
        #[arg(long, default_value_t = 120)]
        hold: u64,
        /// Drag target: --to X,Y
        #[arg(long)]
        to: Option<String>,
        /// Target region in touch space: ORIGIN_X,ORIGIN_Y,SCALE_X,SCALE_Y
        #[arg(long)]
        region: Option<String>,
        /// X eksenini aynala
        #[arg(long)]
        invert_x: bool,
        /// Y eksenini aynala
        #[arg(long)]
        invert_y: bool,
        /// Wait before touching (seconds) — to bring the target window forward
        #[arg(long, default_value_t = 0)]
        delay: u64,
        /// Force the old uinput path (default: the Waydroid touch pipe)
        #[arg(long)]
        uinput: bool,
    },
    /// Try a profile with a real keyboard (NO injection into Android)
    Test {
        /// Profile file (.toml)
        profile: std::path::PathBuf,
        /// Use a specific device (default: the first keyboard)
        #[arg(short, long)]
        device: Option<std::path::PathBuf>,
        /// Grab the device — keys do not reach the desktop
        #[arg(short, long)]
        grab: bool,
        /// Screen width (for pixel conversion)
        #[arg(long, default_value_t = 1920)]
        width: u32,
        /// Screen height
        #[arg(long, default_value_t = 1080)]
        height: u32,
        /// ACTUALLY inject the touches (virtual touchscreen)
        #[arg(short, long)]
        inject: bool,
    },
}

#[derive(Subcommand)]
enum PerfAction {
    /// Read and report the current state of the levers
    Status,
}

#[derive(Subcommand)]
enum VideoAction {
    /// What decodes video in Android, what the host can decode, and the gap
    Status,
    /// Measure what software decode costs against hardware, on the host
    Probe {
        /// Passes per decoder. More passes, less noise.
        #[arg(short, long, default_value_t = 20)]
        loops: u32,
    },
    /// Compare what a RUNNING Android registered against what the image declares
    Verify,
}

#[derive(Subcommand)]
enum SessionAction {
    /// Start the session (detached from the terminal)
    Start,
    /// Stop the session
    Stop,
    /// Restart the session
    Restart,
    /// Status summary
    Status,
    /// Detailed health check — reports which signal is down
    Health,
    /// Waydroid penceresini tam ekran yap
    Fullscreen,
}

/// Talk to the daemon if present; otherwise fall back to Waydroid directly.
///
/// Direct mode is a deliberate trade-off: `liw` should be useful on a system
/// without the daemon installed. But there is NO automatic recovery in it.
async fn manager() -> Option<zbus::Proxy<'static>> {
    let conn = Connection::session().await.ok()?;
    let p = zbus::Proxy::new(&conn, BUS_NAME, OBJ_PATH, BUS_NAME).await.ok()?;
    p.introspect().await.ok()?;
    Some(p)
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let action = match cli.cmd {
        Cmd::Bench { package, duration } => return bench::run(package, duration).await,
        Cmd::Trace { package, duration, jank_ms } =>
            return trace::run(package, duration, jank_ms).await,
        Cmd::Perf { action } => return match action {
            PerfAction::Status => perf::status(),
        },
        Cmd::Video { action } => return match action {
            VideoAction::Status => video::status(),
            VideoAction::Probe { loops } => video::probe(loops),
            VideoAction::Verify => video::verify().await,
        },
        Cmd::Profile { action } => {
            return match action {
                ProfileAction::List => profile::list(),
                ProfileAction::Show { package } => profile::show(&package),
                ProfileAction::Which => profile::which().await,
                ProfileAction::Edit { package, port } => editor::run(&package, port).await,
                ProfileAction::Set { package, binding, x, y, field } =>
                    profile::set_coord(&package, &binding, &field, x, y),
                ProfileAction::Poke { package, binding, delay } =>
                    profile::poke_binding(&package, &binding, delay).await,
                ProfileAction::Install { force, from } => profile::install(force, from),
            };
        }
        Cmd::Keymap { action } => {
            return match action {
                KeymapAction::Devices => keymap::list_devices(),
                KeymapAction::Start { grab } => keymap::daemon_start(grab).await,
                KeymapAction::Stop => keymap::daemon_stop().await,
                KeymapAction::Status => keymap::daemon_status().await,
                KeymapAction::Run { grab, poll } => keymap::run(grab, poll).await,
                KeymapAction::Overlay { off } => keymap::overlay(!off).await,
                KeymapAction::Sweep { axis, count, gap } => keymap::sweep(axis, count, gap).await,
                KeymapAction::Detect { save, mouse, hotkey } =>
                    keymap::detect(save, mouse, hotkey).await,
                KeymapAction::Watch { device } => keymap::watch(device).await,
                KeymapAction::Poke { x, y, hold, to, region, invert_x, invert_y, delay, uinput } => {
                    let drag = match to {
                        Some(s) => {
                            let (a, b) = s.split_once(',')
                                .context("--to format: X,Y  (e.g. 0.2,0.5)")?;
                            Some((a.trim().parse()?, b.trim().parse()?))
                        }
                        None => None,
                    };
                    let mut map = liw_core::input::ScreenMap::default();
                    if let Some(r) = region {
                        let v: Vec<f32> = r.split(',')
                            .map(|p| p.trim().parse::<f32>())
                            .collect::<Result<_, _>>()
                            .context("--region format: OX,OY,SX,SY")?;
                        anyhow::ensure!(v.len() == 4, "--region needs four numbers: OX,OY,SX,SY");
                        map.origin_x = v[0]; map.origin_y = v[1];
                        map.scale_x = v[2];  map.scale_y = v[3];
                    }
                    map.invert_x = invert_x;
                    map.invert_y = invert_y;
                    keymap::poke(x, y, hold, drag, map, delay, uinput).await
                }
                KeymapAction::Test { profile, device, grab, width, height, inject } =>
                    keymap::test_profile(profile, device, grab, (width, height), inject).await,
            };
        }
        Cmd::Session { action } => action,
    };
    let proxy = manager().await;
    if proxy.is_none() {
        eprintln!("warning: liwd is not running — direct mode, no automatic recovery");
    }
    let sup = Supervisor::new(SupervisorConfig::default()).with_helper().await;

    match action {
        SessionAction::Start => {
            let r = match &proxy {
                Some(p) => p.call::<_, _, ()>("Start", &()).await
                    .map_err(anyhow::Error::from).context("Start call"),
                None => sup.start_detached().await
                    .map_err(anyhow::Error::from).context("starting the session"),
            };
            // Asking for a start is not the same as starting. The D-Bus
            // call returns as soon as the request is accepted, and waydroid
            // fails afterwards in its own process — so reporting success from
            // the call alone announces a session that is not there.
            let requested = r.is_ok();
            let mut up = false;
            if requested {
                for _ in 0..30 {
                    if sup.status().await.map(|s| s.session_running()).unwrap_or(false) {
                        up = true;
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            }
            if up {
                println!("session started");
            } else {
                // A failed start gets blamed on Waydroid or on the firewall,
                // because that is what the error text talks about. Check the
                // host first: a kernel update that removed the running
                // kernel's modules produces exactly this, and nothing in the
                // visible error mentions modules.
                eprintln!("the session did not come up");
                if let Some(s) = liw_core::host::check_modules() {
                    eprintln!("\n  The host explains it: {}\n", s.summary());
                    eprintln!("  Waydroid's NAT rule needs nft_masq. That module is");
                    eprintln!("  not loaded and can no longer be loaded, so the rule");
                    eprintln!("  fails and the bridge is never created.\n");
                    eprintln!("  Reboot into {}, then start the session again.\n",
                              s.available.last().map(String::as_str).unwrap_or("the new kernel"));
                }
                if let Err(e) = r { return Err(e); }
                anyhow::bail!("session start was accepted but the session is not running");
            }
        }
        SessionAction::Stop => {
            match &proxy {
                Some(p) => p.call::<_, _, ()>("Stop", &()).await.context("Stop call")?,
                None => sup.stop().await.context("session durdurma")?,
            }
            println!("session durduruldu");
        }
        SessionAction::Restart => {
            match &proxy {
                Some(p) => p.call::<_, _, ()>("Restart", &()).await.context("Restart call")?,
                None => sup.recover().await.context("restarting")?,
            }
            println!("session restarted");
        }
        SessionAction::Status => {
            let s = sup.status().await.context("could not read status")?;
            println!("Session   : {}", s.session);
            println!("Container : {}", s.container);
            println!("IP        : {}", s.ip.as_deref().unwrap_or("-"));
            if let Some(p) = &proxy {
                if let Ok(st) = p.get_property::<String>("State").await {
                    println!("liwd      : {st}");
                }
            } else {
                println!("liwd      : not running");
            }
        }
        SessionAction::Fullscreen => {
            let p = proxy.as_ref()
                .context("liwd is not running — systemctl --user status liwd")?;
            let ok: bool = p.call("Fullscreen", &()).await.context("Fullscreen call")?;
            let json: String = p.call("WindowGeometry", &()).await.unwrap_or_default();
            if ok {
                println!("window is fullscreen");
            } else {
                println!("could not make it fullscreen");
            }
            if !json.is_empty() { println!("geometri: {json}"); }
        }
        SessionAction::Health => {
            let h: Health = match &proxy {
                Some(p) => {
                    let json: String = p.call("Health", &()).await.context("Health call")?;
                    serde_json::from_str(&json).context("could not parse health data")?
                }
                None => sup.health().await,
            };
            let mark = |b: bool| if b { "OK  " } else { "HATA" };
            println!("  {} session running", mark(h.session_running));
            println!("  {} container running", mark(h.container_running));
            println!("  {} composer HAL alive", mark(h.composer_alive));
            println!("  {} composer connection fresh", mark(!h.composer_stale));
            println!("  {} Android boot completed", mark(h.boot_completed));
            println!("  {} IP assigned", mark(h.has_ip));
            println!();
            if h.is_healthy() {
                println!("session healthy");
            } else {
                println!("SORUNLAR:");
                for f in h.failures() { println!("  - {f}"); }
                if h.composer_stale {
                    println!();
                    println!("composer restarted after the session. The processes look");
                    println!("alive but the binder connection is stale: no window appears");
                    println!("and 'waydroid app launch' returns 'Sending reply failed'.");
                    println!("To recover: liw session restart");
                }
                if !h.composer_alive {
                    println!();
                    println!("composer death is the root of the crash chain:");
                    println!("  composer -> SurfaceFlinger SIGABRT -> system_server -> every app");
                    println!("To recover: liw session restart");
                }
                std::process::exit(1);
            }
        }
    }
    Ok(())
}
