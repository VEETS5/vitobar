use sysinfo::{System};
use super::bluetooth::{BluetoothState, get_bluetooth};

#[derive(Debug, Clone)]
pub struct SysStats {
    pub cpu_pct:        f32,
    pub battery_pct:    Option<f32>,
    pub volume_pct:     u32,
    pub brightness_pct: u32,
    pub ram_gb:         f32,   // used RAM in GiB
    pub bluetooth:      BluetoothState,
}

pub struct SysMonitor {
    sys: System,
}

impl SysMonitor {
    pub fn new() -> Self {
        let mut sys = System::new_all();
        sys.refresh_all();
        Self { sys }
    }

    pub fn refresh(&mut self) -> SysStats {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();

        let cpu_pct = self.sys.cpus().iter().map(|c| c.cpu_usage()).sum::<f32>()
            / self.sys.cpus().len() as f32;
        let ram_gb         = self.sys.used_memory() as f32 / 1_073_741_824.0;
        let battery_pct    = get_battery();
        let volume_pct     = get_volume();
        let brightness_pct = get_brightness();

        let bluetooth = get_bluetooth();
        SysStats { cpu_pct, battery_pct, volume_pct, brightness_pct, ram_gb, bluetooth }
    }
}

fn get_brightness() -> u32 {
    let current = std::fs::read_to_string(
        "/sys/class/backlight/intel_backlight/brightness"
    ).ok().and_then(|s| s.trim().parse::<u32>().ok()).unwrap_or(0);

    let max = std::fs::read_to_string(
        "/sys/class/backlight/intel_backlight/max_brightness"
    ).ok().and_then(|s| s.trim().parse::<u32>().ok()).unwrap_or(1);

    ((current as f32 / max as f32) * 100.0) as u32
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
