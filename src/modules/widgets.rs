// Data sources for the optional left-of-center bar widgets:
// now-playing (playerctl/MPRIS), weather (wttr.in via curl), a cava audio
// spectrum, and network throughput. Mirrors the tray.rs threading pattern
// (Arc<Mutex<…>> + AtomicBool dirty flag) for the async sources.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub type Shared<T> = Arc<Mutex<T>>;

#[derive(Debug, Clone, Default)]
pub struct MediaInfo {
    pub title:   String,
    pub artist:  String,
    pub playing: bool,
    pub present: bool, // a player exists
}

#[derive(Debug, Clone)]
pub struct WeatherInfo {
    pub code: u32, // WWO weather code
    pub hi:   i32,
    pub lo:   i32,
}

/// True if `cmd` is on PATH.
pub fn command_exists(cmd: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {} >/dev/null 2>&1", cmd)])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ── Network throughput sampler (synchronous) ─────────────────────────────────

pub struct NetSampler {
    last_rx: u64,
    last_tx: u64,
    last:    Instant,
    pub up:   u64, // bytes/sec transmitted
    pub down: u64, // bytes/sec received
}

fn read_net_totals() -> (u64, u64) {
    let content = std::fs::read_to_string("/proc/net/dev").unwrap_or_default();
    let (mut rx, mut tx) = (0u64, 0u64);
    for line in content.lines().skip(2) {
        let Some((iface, rest)) = line.split_once(':') else { continue };
        if iface.trim() == "lo" { continue; }
        let cols: Vec<&str> = rest.split_whitespace().collect();
        if cols.len() >= 9 {
            rx += cols[0].parse::<u64>().unwrap_or(0);
            tx += cols[8].parse::<u64>().unwrap_or(0);
        }
    }
    (rx, tx)
}

impl NetSampler {
    pub fn new() -> Self {
        let (rx, tx) = read_net_totals();
        Self { last_rx: rx, last_tx: tx, last: Instant::now(), up: 0, down: 0 }
    }

    pub fn sample(&mut self) {
        let (rx, tx) = read_net_totals();
        let dt = self.last.elapsed().as_secs_f64().max(0.001);
        self.down = ((rx.saturating_sub(self.last_rx)) as f64 / dt) as u64;
        self.up   = ((tx.saturating_sub(self.last_tx)) as f64 / dt) as u64;
        self.last_rx = rx;
        self.last_tx = tx;
        self.last = Instant::now();
    }
}

// ── Aggregate state held on the bar ──────────────────────────────────────────

pub struct WidgetData {
    pub media:        Shared<MediaInfo>,
    pub media_dirty:  Arc<AtomicBool>,
    pub weather:      Shared<Option<WeatherInfo>>,
    pub weather_dirty: Arc<AtomicBool>,
    pub weather_loc:   Shared<String>,
    pub weather_units: Shared<String>,
    pub weather_refetch: Arc<AtomicBool>,
    pub cava: Shared<Vec<u8>>,
    pub net:  NetSampler,
}

impl WidgetData {
    pub fn new(location: &str, units: &str) -> Self {
        Self {
            media:        Arc::new(Mutex::new(MediaInfo::default())),
            media_dirty:  Arc::new(AtomicBool::new(false)),
            weather:      Arc::new(Mutex::new(None)),
            weather_dirty: Arc::new(AtomicBool::new(false)),
            weather_loc:   Arc::new(Mutex::new(location.to_string())),
            weather_units: Arc::new(Mutex::new(units.to_string())),
            weather_refetch: Arc::new(AtomicBool::new(true)),
            cava: Arc::new(Mutex::new(Vec::new())),
            net:  NetSampler::new(),
        }
    }

    /// Spawn the always-on background pollers (media + weather). Cheap; safe to
    /// call once at startup regardless of which widgets are enabled.
    pub fn start_background(&self) {
        spawn_media(self.media.clone(), self.media_dirty.clone());
        spawn_weather(
            self.weather.clone(),
            self.weather_dirty.clone(),
            self.weather_loc.clone(),
            self.weather_units.clone(),
            self.weather_refetch.clone(),
        );
    }
}

// ── Media (playerctl --follow) ───────────────────────────────────────────────

fn spawn_media(media: Shared<MediaInfo>, dirty: Arc<AtomicBool>) {
    std::thread::spawn(move || loop {
        let mut child = match Command::new("playerctl")
            .args(["--follow", "--format", "{{status}}\t{{title}}\t{{artist}}", "metadata"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => {
                // playerctl not installed — stop trying.
                return;
            }
        };

        if let Some(out) = child.stdout.take() {
            for line in BufReader::new(out).lines() {
                let Ok(line) = line else { break };
                let mut parts = line.splitn(3, '\t');
                let status = parts.next().unwrap_or("");
                let title  = parts.next().unwrap_or("");
                let artist = parts.next().unwrap_or("");
                let info = MediaInfo {
                    title:   title.to_string(),
                    artist:  artist.to_string(),
                    playing: status == "Playing",
                    present: !status.is_empty(),
                };
                if let Ok(mut g) = media.lock() { *g = info; }
                dirty.store(true, Ordering::Release);
            }
        }
        let _ = child.wait();
        // Player went away: clear and retry after a pause.
        if let Ok(mut g) = media.lock() { *g = MediaInfo::default(); }
        dirty.store(true, Ordering::Release);
        std::thread::sleep(Duration::from_secs(3));
    });
}

// ── Weather (wttr.in via curl + serde_json) ──────────────────────────────────

fn spawn_weather(
    weather: Shared<Option<WeatherInfo>>,
    dirty: Arc<AtomicBool>,
    loc: Shared<String>,
    units: Shared<String>,
    refetch: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        let mut last_fetch = Instant::now() - Duration::from_secs(3600);
        loop {
            let due = refetch.swap(false, Ordering::Acquire)
                || last_fetch.elapsed() >= Duration::from_secs(900);
            if due {
                let location = loc.lock().map(|g| g.clone()).unwrap_or_default();
                let unit = units.lock().map(|g| g.clone()).unwrap_or_else(|_| "C".into());
                last_fetch = Instant::now();
                if location.trim().is_empty() {
                    if let Ok(mut g) = weather.lock() { *g = None; }
                    dirty.store(true, Ordering::Release);
                } else if let Some(info) = fetch_weather(&location, &unit) {
                    if let Ok(mut g) = weather.lock() { *g = Some(info); }
                    dirty.store(true, Ordering::Release);
                }
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    });
}

fn fetch_weather(location: &str, units: &str) -> Option<WeatherInfo> {
    let loc_enc = location.trim().replace(' ', "+");
    let url = format!("wttr.in/{}?format=j1", loc_enc);
    let out = Command::new("curl")
        .args(["-s", "--max-time", "15", &url])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let code = v["current_condition"][0]["weatherCode"]
        .as_str()?
        .parse::<u32>()
        .ok()?;
    let w0 = &v["weather"][0];
    let (hi_key, lo_key) = if units.eq_ignore_ascii_case("F") {
        ("maxtempF", "mintempF")
    } else {
        ("maxtempC", "mintempC")
    };
    let hi = w0[hi_key].as_str()?.parse::<i32>().ok()?;
    let lo = w0[lo_key].as_str()?.parse::<i32>().ok()?;
    Some(WeatherInfo { code, hi, lo })
}

/// Nerd Font glyph candidates (first that the font has wins) for a WWO weather
/// code. Resolved against the loaded font by the renderer.
pub fn weather_candidates(code: u32) -> &'static [char] {
    match code {
        113 => &['\u{e30d}', '\u{f185}'],                 // sunny / clear
        116 => &['\u{e302}', '\u{f185}'],                 // partly cloudy
        119 | 122 => &['\u{e33d}', '\u{f0c2}'],           // cloudy / overcast
        143 | 248 | 260 => &['\u{e313}', '\u{f0c2}'],     // mist / fog
        200 | 386 | 389 | 392 | 395 => &['\u{e31d}', '\u{f0e7}'], // thunder
        // snow / sleet
        179 | 182 | 185 | 227 | 230 | 281 | 284 | 311 | 314 | 317 | 320 | 323
        | 326 | 329 | 332 | 335 | 338 | 350 | 362 | 365 | 368 | 371 | 374 | 377
            => &['\u{e31a}', '\u{f2dc}'],
        // everything else = rain / drizzle / showers
        _ => &['\u{e318}', '\u{f043}'],
    }
}

/// Format a byte/sec rate compactly (e.g. "1.2M", "320K", "12B").
pub fn fmt_rate(bps: u64) -> String {
    let b = bps as f64;
    if b >= 1_048_576.0 {
        format!("{:.1}M", b / 1_048_576.0)
    } else if b >= 1024.0 {
        format!("{:.0}K", b / 1024.0)
    } else {
        format!("{}B", bps)
    }
}

// ── Cava audio spectrum ──────────────────────────────────────────────────────

pub struct CavaHandle {
    child: Child,
    stop:  Arc<AtomicBool>,
}

impl CavaHandle {
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Start `cava` in raw binary mode with `bars` bars, streaming bar heights
/// (0–255) into `out`. Returns None if cava is missing or fails to launch.
pub fn start_cava(bars: usize, out: Shared<Vec<u8>>) -> Option<CavaHandle> {
    if !command_exists("cava") {
        return None;
    }
    // Instance-scoped config path (includes our PID) so concurrent bars — e.g.
    // one process per monitor — don't share a file or reap each other's cava.
    // Crash recovery is handled by SIGPIPE (cava dies when our read end closes),
    // so this pkill only ever targets a cava left by THIS process.
    let cfg_name = format!("vitobar-cava.{}.conf", std::process::id());
    let _ = Command::new("pkill").args(["-f", &cfg_name]).status();
    let cfg_path = std::env::temp_dir().join(&cfg_name);
    let cfg = format!(
        "[general]\nbars = {bars}\nframerate = 30\n\n\
         [output]\nmethod = raw\nraw_target = /dev/stdout\n\
         data_format = binary\nbit_format = 8\nchannels = mono\n"
    );
    std::fs::File::create(&cfg_path).ok()?.write_all(cfg.as_bytes()).ok()?;

    let mut child = Command::new("cava")
        .arg("-p")
        .arg(&cfg_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();

    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut frame = vec![0u8; bars];
        loop {
            if stop2.load(Ordering::Acquire) {
                break;
            }
            match reader.read_exact(&mut frame) {
                Ok(()) => {
                    if let Ok(mut g) = out.lock() {
                        *g = frame.clone();
                    }
                }
                Err(_) => break,
            }
        }
    });

    Some(CavaHandle { child, stop })
}
