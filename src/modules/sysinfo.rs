use sysinfo::{System, Components, ProcessRefreshKind};
use super::bluetooth::{BluetoothState, get_bluetooth};

#[derive(Debug, Clone)]
pub struct SysStats {
    pub cpu_pct:        f32,
    pub cpu_temp:       Option<f32>,
    pub gpu_temp:       Option<f32>,
    pub battery_pct:    Option<f32>,
    pub volume_pct:     u32,
    pub brightness_pct: Option<u32>,
    pub ram_gb:         f32,   // used RAM in GiB
    pub bluetooth:      BluetoothState,
    pub background_apps: Vec<String>,
}

pub struct SysMonitor {
    sys: System,
    components: Components,
}

impl SysMonitor {
    pub fn new() -> Self {
        let mut sys = System::new();
        sys.refresh_cpu_usage();
        sys.refresh_memory();
        let components = Components::new_with_refreshed_list();
        Self { sys, components }
    }

    pub fn refresh(&mut self) -> SysStats {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        self.components.refresh();

        let cpu_pct = self.sys.cpus().iter().map(|c| c.cpu_usage()).sum::<f32>()
            / self.sys.cpus().len() as f32;
        let ram_gb         = self.sys.used_memory() as f32 / 1_073_741_824.0;
        let battery_pct    = get_battery();
        let volume_pct     = get_volume();
        let brightness_pct = get_brightness();
        let (cpu_temp, gpu_temp) = get_temps(&self.components);

        let bluetooth = get_bluetooth();
        SysStats { cpu_pct, cpu_temp, gpu_temp, battery_pct, volume_pct, brightness_pct, ram_gb, bluetooth, background_apps: Vec::new() }
    }

    /// Detect GUI apps running as processes but not present as niri windows.
    pub fn detect_background_apps(&mut self, window_app_ids: &[String]) -> Vec<String> {
        self.sys.refresh_processes_specifics(ProcessRefreshKind::new());

        // Map process names to canonical app_ids for icon lookup
        const KNOWN_APPS: &[(&[&str], &str)] = &[
            (&["Discord", "discord", "vesktop", "Vesktop"], "discord"),
            (&["steam", "Steam"], "steam"),
            (&["spotify", "Spotify"], "spotify"),
            (&["upc", "UbisoftConnect", "uplay"], "ubisoft"),
            (&["telegram-desktop", "telegram", "Telegram"], "telegram"),
            (&["slack", "Slack"], "slack"),
            (&["obs", "OBS"], "obs"),
            (&["signal-desktop", "Signal"], "signal"),
            (&["thunderbird", "Thunderbird"], "thunderbird"),
            (&["lutris", "Lutris"], "lutris"),
            (&["heroic", "Heroic"], "heroic"),
        ];

        let mut found: Vec<String> = Vec::new();
        for (proc_names, app_id) in KNOWN_APPS {
            let running = self.sys.processes().values().any(|p| {
                let name = p.name();
                proc_names.iter().any(|pn| name.contains(pn))
            });
            if running {
                // Only include if no niri window has this app_id
                let has_window = window_app_ids.iter().any(|wid| {
                    let wid_lower = wid.to_ascii_lowercase();
                    wid_lower.contains(app_id)
                });
                if !has_window {
                    found.push(app_id.to_string());
                }
            }
        }
        found
    }
}

fn get_brightness() -> Option<u32> {
    const BACKLIGHT_PATHS: &[&str] = &[
        "/sys/class/backlight/intel_backlight",
        "/sys/class/backlight/amdgpu_bl0",
        "/sys/class/backlight/acpi_video0",
    ];
    for base in BACKLIGHT_PATHS {
        let current = std::fs::read_to_string(format!("{}/brightness", base))
            .ok().and_then(|s| s.trim().parse::<u32>().ok());
        let max = std::fs::read_to_string(format!("{}/max_brightness", base))
            .ok().and_then(|s| s.trim().parse::<u32>().ok());
        if let (Some(cur), Some(mx)) = (current, max) {
            if mx > 0 {
                return Some(((cur as f32 / mx as f32) * 100.0) as u32);
            }
        }
    }
    // Fallback: brightnessctl
    std::process::Command::new("brightnessctl")
        .args(["info", "-m"])
        .output()
        .ok()
        .and_then(|o| {
            let s = String::from_utf8_lossy(&o.stdout);
            // Format: "device,class,current,max,percentage"
            s.split(',').nth(3).and_then(|p| p.trim_end_matches('%').trim().parse::<u32>().ok())
        })
}

fn get_temps(components: &Components) -> (Option<f32>, Option<f32>) {
    let mut cpu_temp: Option<f32> = None;
    let mut gpu_temp: Option<f32> = None;

    for c in components.iter() {
        let label = c.label().to_ascii_lowercase();
        let temp = c.temperature();
        if temp <= 0.0 { continue; }

        // CPU: look for coretemp/k10temp/Tctl/Package id/Tdie
        if cpu_temp.is_none() {
            if label.contains("package id")
                || label.contains("tctl")
                || label.contains("tdie")
                || label.contains("coretemp")
                || label.contains("k10temp")
                || label.contains("cpu")
            {
                cpu_temp = Some(temp);
            }
        }

        // GPU: look for amdgpu/nvidia/intel igpu (i915/xe)/edge/junction
        if gpu_temp.is_none() {
            if label.contains("amdgpu")
                || label.contains("nvidia")
                || label.contains("i915")
                || label.contains("xe")
                || label.contains("edge")
                || label.contains("junction")
                || label.contains("gpu")
            {
                gpu_temp = Some(temp);
            }
        }
    }

    // Fallback for nvidia: nvidia-smi
    if gpu_temp.is_none() {
        gpu_temp = std::process::Command::new("nvidia-smi")
            .args(["--query-gpu=temperature.gpu", "--format=csv,noheader,nounits"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<f32>().ok());
    }

    (cpu_temp, gpu_temp)
}

fn get_battery() -> Option<f32> {
    // Read from /sys/class/power_supply/BAT0/capacity
    let path = "/sys/class/power_supply/BAT0/capacity";
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse::<f32>().ok())
}

fn get_volume() -> u32 {
    // Call wpctl and parse output
    let output = std::process::Command::new("wpctl")
        .args(["get-volume", "@DEFAULT_AUDIO_SINK@"])
        .output();

    match output {
        Ok(out) => {
            let s = String::from_utf8_lossy(&out.stdout);
            // output looks like: "Volume: 0.60"
            s.split_whitespace()
                .last()
                .and_then(|v| v.parse::<f32>().ok())
                .map(|v| (v * 100.0) as u32)
                .unwrap_or(0)
        }
        Err(_) => 0,
    }
}
