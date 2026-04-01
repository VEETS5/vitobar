use super::categories::Category;

#[derive(Debug, Clone)]
pub enum Widget {
    Slider {
        label:        String,
        value:        f32,   // 0.0–1.0
        cmd_template: &'static str,  // "{}" replaced with integer percent 0–100
    },
    ConfigSlider {
        label: String,
        value: f32,   // 0.0–1.0 (normalized)
        min:   f32,
        max:   f32,
        key:   &'static str,  // config.toml key
    },
    Toggle {
        label:   String,
        value:   bool,
        cmd_on:  &'static str,
        cmd_off: &'static str,
    },
    InfoRow {
        label: String,
        value: String,
    },
    Button {
        label: String,
        cmd:   String,
    },
    SectionHeader {
        label: String,
    },
}

#[derive(Debug)]
pub enum WidgetResult {
    None,
    ConfigUpdate { key: &'static str, value: f32 },
}

fn read_hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "nixos".to_string())
}

fn get_nix_generation() -> String {
    let out = std::process::Command::new("nix-env")
        .args(["--list-generations"])
        .output()
        .ok();
    out.and_then(|o| {
        let text = String::from_utf8_lossy(&o.stdout).to_string();
        text.lines().last().map(|l| l.trim().to_string())
    })
    .unwrap_or_else(|| "unknown".to_string())
}

fn get_kernel_version() -> String {
    let out = std::process::Command::new("uname")
        .arg("-r").output().ok();
    out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into())
}

fn get_uptime() -> String {
    let out = std::process::Command::new("uptime")
        .arg("-p").output().ok();
    out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into())
}

fn get_cpu_model() -> String {
    let content = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    content.lines()
        .find(|l| l.starts_with("model name"))
        .and_then(|l| l.splitn(2, ':').nth(1))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".into())
}

fn get_total_ram_gb() -> String {
    let content = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let kb = content.lines()
        .find(|l| l.starts_with("MemTotal:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    format!("{:.1} GB", kb as f64 / 1_048_576.0)
}

fn get_disk_usage(path: &str) -> String {
    let out = std::process::Command::new("df")
        .args(["-h", "--output=used,size,pcent", path])
        .output().ok();
    out.and_then(|o| {
        let text = String::from_utf8_lossy(&o.stdout).to_string();
        text.lines().nth(1).map(|l| {
            let parts: Vec<&str> = l.split_whitespace().collect();
            if parts.len() >= 3 {
                format!("{} / {} ({})", parts[0], parts[1], parts[2])
            } else {
                l.trim().to_string()
            }
        })
    }).unwrap_or_else(|| "unknown".into())
}

fn get_power_profile() -> String {
    let out = std::process::Command::new("powerprofilesctl")
        .arg("get").output().ok();
    out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unavailable".into())
}

fn get_battery_status() -> String {
    let try_paths = ["/sys/class/power_supply/BAT0", "/sys/class/power_supply/BAT1"];
    for path in &try_paths {
        let cap = std::fs::read_to_string(format!("{}/capacity", path))
            .ok().and_then(|s| s.trim().parse::<u32>().ok());
        let status = std::fs::read_to_string(format!("{}/status", path))
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "Unknown".into());
        if let Some(pct) = cap {
            return format!("{}% — {}", pct, status);
        }
    }
    "No battery".into()
}

fn get_wifi_ssid() -> String {
    let out = std::process::Command::new("nmcli")
        .args(["-t", "-f", "active,ssid", "dev", "wifi"])
        .output().ok();
    out.and_then(|o| {
        let text = String::from_utf8_lossy(&o.stdout).to_string();
        text.lines()
            .find(|l| l.starts_with("yes:"))
            .map(|l| l.trim_start_matches("yes:").to_string())
    }).unwrap_or_else(|| "Not connected".into())
}

fn get_ip_addresses() -> String {
    let out = std::process::Command::new("ip")
        .args(["-br", "addr"])
        .output().ok();
    out.map(|o| {
        let text = String::from_utf8_lossy(&o.stdout).to_string();
        text.lines()
            .filter(|l| !l.starts_with("lo "))
            .take(2)
            .map(|l| {
                let parts: Vec<&str> = l.split_whitespace().collect();
                if parts.len() >= 3 {
                    format!("{}: {}", parts[0], parts[2].split('/').next().unwrap_or(parts[2]))
                } else if parts.len() >= 2 {
                    format!("{}: {}", parts[0], parts[1])
                } else {
                    l.trim().to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("  ")
    }).unwrap_or_else(|| "unavailable".into())
}

fn get_default_sink_name() -> String {
    let out = std::process::Command::new("wpctl")
        .args(["inspect", "@DEFAULT_AUDIO_SINK@"])
        .output().ok();
    out.and_then(|o| {
        let text = String::from_utf8_lossy(&o.stdout).to_string();
        text.lines()
            .find(|l| l.contains("node.nick") || l.contains("node.description"))
            .and_then(|l| l.splitn(2, '=').nth(1))
            .map(|s| s.trim().trim_matches('"').to_string())
    }).unwrap_or_else(|| "Default sink".into())
}

fn get_bt_paired_devices() -> Vec<String> {
    let out = std::process::Command::new("bluetoothctl")
        .args(["paired-devices"]).output().ok();
    out.map(|o| {
        String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter_map(|l| {
                let parts: Vec<&str> = l.splitn(3, ' ').collect();
                if parts.len() >= 3 { Some(parts[2].to_string()) } else { None }
            })
            .collect()
    }).unwrap_or_default()
}

fn get_current_wallpaper() -> String {
    // Check swaybg, swww, or niri config for wallpaper path
    let out = std::process::Command::new("pgrep")
        .args(["-a", "swaybg"])
        .output().ok();
    if let Some(o) = out {
        let text = String::from_utf8_lossy(&o.stdout).to_string();
        if let Some(line) = text.lines().next() {
            if let Some(img) = line.split("-i ").nth(1) {
                let path = img.split_whitespace().next().unwrap_or(img);
                return path.to_string();
            }
        }
    }
    let out = std::process::Command::new("swww")
        .args(["query"])
        .output().ok();
    if let Some(o) = out {
        let text = String::from_utf8_lossy(&o.stdout).to_string();
        if let Some(line) = text.lines().next() {
            if let Some(img) = line.split("image: ").nth(1) {
                return img.trim().to_string();
            }
        }
    }
    "Unknown".into()
}

fn get_wallpaper_tool() -> &'static str {
    if std::process::Command::new("which").arg("swww").output()
        .map(|o| o.status.success()).unwrap_or(false) {
        "swww"
    } else if std::process::Command::new("which").arg("swaybg").output()
        .map(|o| o.status.success()).unwrap_or(false) {
        "swaybg"
    } else {
        "none"
    }
}

pub fn build_widgets(cat: Category, config: &crate::config::Config) -> Vec<Widget> {
    match cat {
        Category::Appearance => {
            let bar_h = config.bar_height.unwrap_or(22) as f32;
            let taskbar_h = config.taskbar_height.unwrap_or(22) as f32;
            let font_sz = config.font_size.unwrap_or(11.0);
            vec![
                Widget::SectionHeader { label: "Bar".into() },
                Widget::ConfigSlider {
                    label: "Bar Height".into(),
                    value: (bar_h - 16.0) / 32.0,  // normalize 16..48 to 0..1
                    min: 16.0,
                    max: 48.0,
                    key: "bar_height",
                },
                Widget::ConfigSlider {
                    label: "Taskbar Height".into(),
                    value: (taskbar_h - 16.0) / 32.0,
                    min: 16.0,
                    max: 48.0,
                    key: "taskbar_height",
                },
                Widget::SectionHeader { label: "Typography".into() },
                Widget::ConfigSlider {
                    label: "Font Size".into(),
                    value: (font_sz - 8.0) / 12.0,  // normalize 8..20 to 0..1
                    min: 8.0,
                    max: 20.0,
                    key: "font_size",
                },
                Widget::SectionHeader { label: "Theme".into() },
                Widget::InfoRow {
                    label: "Colors".into(),
                    value: "Managed by Stylix".into(),
                },
                Widget::InfoRow {
                    label: "Config".into(),
                    value: crate::config::config_path().to_string_lossy().to_string(),
                },
            ]
        }

        Category::Display => {
            let brightness = get_brightness_pct();
            vec![
                Widget::SectionHeader { label: "Screen".into() },
                Widget::Slider {
                    label: "Brightness".into(),
                    value: brightness as f32 / 100.0,
                    cmd_template: "brightnessctl set {}%",
                },
                Widget::Toggle {
                    label:   "Night Light".into(),
                    value:   is_night_light_on(),
                    cmd_on:  "hyprsunset -t 4000",
                    cmd_off: "pkill hyprsunset",
                },
            ]
        }

        Category::Audio => {
            let volume = get_volume_pct();
            let muted  = get_muted();
            let mic_vol = get_mic_pct();
            let sink_name = get_default_sink_name();
            vec![
                Widget::SectionHeader { label: "Output".into() },
                Widget::InfoRow {
                    label: "Device".into(),
                    value: sink_name,
                },
                Widget::Slider {
                    label: "Volume".into(),
                    value: volume as f32 / 100.0,
                    cmd_template: "wpctl set-volume @DEFAULT_AUDIO_SINK@ {}%",
                },
                Widget::Toggle {
                    label:   "Mute".into(),
                    value:   muted,
                    cmd_on:  "wpctl set-mute @DEFAULT_AUDIO_SINK@ 1",
                    cmd_off: "wpctl set-mute @DEFAULT_AUDIO_SINK@ 0",
                },
                Widget::SectionHeader { label: "Input".into() },
                Widget::Slider {
                    label: "Mic Volume".into(),
                    value: mic_vol as f32 / 100.0,
                    cmd_template: "wpctl set-volume @DEFAULT_AUDIO_SOURCE@ {}%",
                },
            ]
        }

        Category::Bluetooth => {
            let bt = get_bt_info();
            let mut widgets = vec![
                Widget::SectionHeader { label: "Bluetooth".into() },
                Widget::Toggle {
                    label:   "Power".into(),
                    value:   bt.powered,
                    cmd_on:  "bluetoothctl power on",
                    cmd_off: "bluetoothctl power off",
                },
                Widget::InfoRow {
                    label: "Status".into(),
                    value: bt.status_text,
                },
                Widget::Button {
                    label: "\u{f294}  Open Bluetooth Manager".into(),
                    cmd:   "blueman-manager".into(),
                },
                Widget::Button {
                    label: "\u{f002}  Scan for Devices".into(),
                    cmd:   "bluetoothctl scan on".into(),
                },
            ];
            let paired = get_bt_paired_devices();
            if !paired.is_empty() {
                widgets.push(Widget::SectionHeader { label: "Paired Devices".into() });
                for device in paired.iter().take(5) {
                    widgets.push(Widget::InfoRow {
                        label: "\u{f294}".into(),
                        value: device.clone(),
                    });
                }
            }
            widgets
        }

        Category::Network => {
            let ssid = get_wifi_ssid();
            let ips  = get_ip_addresses();
            vec![
                Widget::SectionHeader { label: "WiFi".into() },
                Widget::InfoRow {
                    label: "SSID".into(),
                    value: ssid,
                },
                Widget::SectionHeader { label: "Addresses".into() },
                Widget::InfoRow {
                    label: "IP".into(),
                    value: ips,
                },
                Widget::Button {
                    label: "\u{f1eb}  Open Network Manager".into(),
                    cmd:   "nm-connection-editor".into(),
                },
            ]
        }

        Category::Wallpaper => {
            let current = get_current_wallpaper();
            let tool = get_wallpaper_tool();
            let mut widgets = vec![
                Widget::SectionHeader { label: "Current Wallpaper".into() },
                Widget::InfoRow {
                    label: "Path".into(),
                    value: current,
                },
                Widget::InfoRow {
                    label: "Backend".into(),
                    value: tool.to_string(),
                },
                Widget::SectionHeader { label: "Actions".into() },
            ];
            match tool {
                "swww" => {
                    widgets.push(Widget::Button {
                        label: "\u{f021}  Initialize swww".into(),
                        cmd: "swww-daemon".into(),
                    });
                    widgets.push(Widget::Button {
                        label: "\u{f03e}  Set Wallpaper (swww)".into(),
                        cmd: "swww img ~/Pictures/wallpaper.png --transition-type fade".into(),
                    });
                }
                "swaybg" => {
                    widgets.push(Widget::Button {
                        label: "\u{f03e}  Set Wallpaper (swaybg)".into(),
                        cmd: "swaybg -i ~/Pictures/wallpaper.png -m fill".into(),
                    });
                }
                _ => {
                    widgets.push(Widget::InfoRow {
                        label: "Note".into(),
                        value: "No wallpaper tool found (swww/swaybg)".into(),
                    });
                }
            }
            widgets.push(Widget::SectionHeader { label: "Tips".into() });
            widgets.push(Widget::InfoRow {
                label: "".into(),
                value: "Set wallpaper in niri config for persistence".into(),
            });
            widgets
        }

        Category::Power => {
            let bat     = get_battery_status();
            let profile = get_power_profile();
            vec![
                Widget::SectionHeader { label: "Battery".into() },
                Widget::InfoRow {
                    label: "Status".into(),
                    value: bat,
                },
                Widget::SectionHeader { label: "Power Profile".into() },
                Widget::InfoRow {
                    label: "Active".into(),
                    value: profile,
                },
                Widget::Button {
                    label: "\u{f0e7}  Performance".into(),
                    cmd:   "powerprofilesctl set performance".into(),
                },
                Widget::Button {
                    label: "\u{f24e}  Balanced".into(),
                    cmd:   "powerprofilesctl set balanced".into(),
                },
                Widget::Button {
                    label: "\u{f06c}  Power Saver".into(),
                    cmd:   "powerprofilesctl set power-saver".into(),
                },
                Widget::SectionHeader { label: "Session".into() },
                Widget::Button {
                    label: "\u{f186}  Suspend".into(),
                    cmd:   "systemctl suspend".into(),
                },
                Widget::Button {
                    label: "\u{f021}  Reboot".into(),
                    cmd:   "systemctl reboot".into(),
                },
                Widget::Button {
                    label: "\u{f011}  Shutdown".into(),
                    cmd:   "systemctl poweroff".into(),
                },
            ]
        }

        Category::System => {
            let hostname   = read_hostname();
            let kernel     = get_kernel_version();
            let uptime     = get_uptime();
            let cpu        = get_cpu_model();
            let ram        = get_total_ram_gb();
            let disk_root  = get_disk_usage("/");
            let disk_home  = get_disk_usage("/home");
            let generation = get_nix_generation();
            vec![
                Widget::SectionHeader { label: "About This Machine".into() },
                Widget::InfoRow { label: "Hostname".into(),  value: hostname.clone() },
                Widget::InfoRow { label: "Kernel".into(),    value: kernel },
                Widget::InfoRow { label: "Uptime".into(),    value: uptime },
                Widget::SectionHeader { label: "Hardware".into() },
                Widget::InfoRow { label: "CPU".into(),       value: cpu },
                Widget::InfoRow { label: "Memory".into(),    value: ram },
                Widget::SectionHeader { label: "Storage".into() },
                Widget::InfoRow { label: "/".into(),         value: disk_root },
                Widget::InfoRow { label: "/home".into(),     value: disk_home },
                Widget::SectionHeader { label: "NixOS".into() },
                Widget::InfoRow {
                    label: "Generation".into(),
                    value: generation,
                },
                Widget::Button {
                    label: "\u{f021}  Flake Update".into(),
                    cmd:   "nix flake update /etc/nixos".into(),
                },
                Widget::Button {
                    label: "\u{f1b2}  Rebuild Switch".into(),
                    cmd:   format!("sudo nixos-rebuild switch --flake /etc/nixos#{}", hostname),
                },
                Widget::Button {
                    label: "\u{f019}  Rebuild Boot".into(),
                    cmd:   format!("sudo nixos-rebuild boot --flake /etc/nixos#{}", hostname),
                },
                Widget::Button {
                    label: "\u{f1f8}  Garbage Collect".into(),
                    cmd:   "nix store gc".into(),
                },
                Widget::Button {
                    label: "\u{f0b0}  Optimise Store".into(),
                    cmd:   "nix store optimise".into(),
                },
            ]
        }
    }
}

pub fn apply_widget_action(widget: &mut Widget, new_value: f32) -> WidgetResult {
    match widget {
        Widget::Slider { value, cmd_template, .. } => {
            *value = new_value.clamp(0.0, 1.0);
            let pct = (*value * 100.0).round() as u32;
            let cmd = cmd_template.replace("{}", &pct.to_string());
            run_cmd(&cmd);
            WidgetResult::None
        }
        Widget::ConfigSlider { value, min, max, key, .. } => {
            *value = new_value.clamp(0.0, 1.0);
            let actual = *min + (*max - *min) * *value;
            WidgetResult::ConfigUpdate { key, value: actual }
        }
        Widget::Toggle { value, cmd_on, cmd_off, .. } => {
            *value = new_value > 0.5;
            let cmd = if *value { *cmd_on } else { *cmd_off };
            run_cmd(cmd);
            WidgetResult::None
        }
        Widget::Button { cmd, .. } => {
            run_cmd(cmd);
            WidgetResult::None
        }
        Widget::InfoRow { .. } | Widget::SectionHeader { .. } => WidgetResult::None,
    }
}

fn run_cmd(cmd: &str) {
    let parts: Vec<&str> = cmd.splitn(2, ' ').collect();
    if parts.is_empty() { return; }
    let prog = parts[0];
    let mut c = std::process::Command::new(prog);
    if parts.len() > 1 {
        c.args(parts[1].split_whitespace());
    }
    c.spawn().ok();
}

fn is_night_light_on() -> bool {
    std::process::Command::new("pgrep")
        .arg("hyprsunset")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// ── System state readers ──────────────────────────────────────────────────────

fn get_brightness_pct() -> u32 {
    let try_paths = [
        "/sys/class/backlight/intel_backlight",
        "/sys/class/backlight/amdgpu_bl0",
        "/sys/class/backlight/acpi_video0",
    ];
    for base in &try_paths {
        let current = std::fs::read_to_string(format!("{}/brightness", base))
            .ok().and_then(|s| s.trim().parse::<u32>().ok());
        let max = std::fs::read_to_string(format!("{}/max_brightness", base))
            .ok().and_then(|s| s.trim().parse::<u32>().ok());
        if let (Some(cur), Some(max)) = (current, max) {
            if max > 0 { return ((cur as f32 / max as f32) * 100.0) as u32; }
        }
    }
    // Try brightnessctl as fallback
    let out = std::process::Command::new("brightnessctl")
        .args(["get"]).output().ok();
    let current = out.and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<u32>().ok()).unwrap_or(0);
    let out = std::process::Command::new("brightnessctl")
        .args(["max"]).output().ok();
    let max = out.and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<u32>().ok()).unwrap_or(1);
    if max > 0 { ((current as f32 / max as f32) * 100.0) as u32 } else { 0 }
}

fn get_volume_pct() -> u32 {
    let out = std::process::Command::new("wpctl")
        .args(["get-volume", "@DEFAULT_AUDIO_SINK@"])
        .output()
        .ok();
    out.and_then(|o| {
        let s = String::from_utf8_lossy(&o.stdout).to_string();
        s.split_whitespace().last()
            .and_then(|v| v.parse::<f32>().ok())
            .map(|v| (v * 100.0) as u32)
    }).unwrap_or(0)
}

fn get_muted() -> bool {
    let out = std::process::Command::new("wpctl")
        .args(["get-volume", "@DEFAULT_AUDIO_SINK@"])
        .output()
        .ok();
    out.map(|o| String::from_utf8_lossy(&o.stdout).contains("[MUTED]"))
        .unwrap_or(false)
}

fn get_mic_pct() -> u32 {
    let out = std::process::Command::new("wpctl")
        .args(["get-volume", "@DEFAULT_AUDIO_SOURCE@"])
        .output()
        .ok();
    out.and_then(|o| {
        let s = String::from_utf8_lossy(&o.stdout).to_string();
        s.split_whitespace().last()
            .and_then(|v| v.parse::<f32>().ok())
            .map(|v| (v * 100.0) as u32)
    }).unwrap_or(0)
}

struct BtInfo {
    powered:     bool,
    status_text: String,
}

fn get_bt_info() -> BtInfo {
    let show = std::process::Command::new("bluetoothctl")
        .args(["show"]).output().ok();
    let powered = show.as_ref()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("Powered: yes"))
        .unwrap_or(false);

    if !powered {
        return BtInfo { powered: false, status_text: "Off".into() };
    }

    let info = std::process::Command::new("bluetoothctl")
        .args(["info"]).output().ok();
    if let Some(out) = info {
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        if text.contains("Connected: yes") {
            let name = text.lines()
                .find(|l| l.trim_start().starts_with("Name:"))
                .and_then(|l| l.splitn(2, ':').nth(1))
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|| "device".into());
            return BtInfo { powered: true, status_text: format!("Connected: {}", name) };
        }
    }
    BtInfo { powered: true, status_text: "On, no device connected".into() }
}
