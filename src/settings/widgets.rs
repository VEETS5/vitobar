use super::categories::Category;

#[derive(Debug, Clone)]
pub enum Widget {
    Slider {
        label:        String,
        value:        f32,   // 0.0–1.0
        cmd_template: &'static str,  // "{}" replaced with integer percent 0–100
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
                // "Device AA:BB:CC:DD:EE:FF Name"
                let parts: Vec<&str> = l.splitn(3, ' ').collect();
                if parts.len() >= 3 { Some(parts[2].to_string()) } else { None }
            })
            .collect()
    }).unwrap_or_default()
}

pub fn build_widgets(cat: Category) -> Vec<Widget> {
    match cat {
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
                    value:   false,
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
                    label: "Open Bluetooth Manager".into(),
                    cmd:   "blueman-manager".into(),
                },
                Widget::Button {
                    label: "Scan for Devices".into(),
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
                    label: "Open Network Manager".into(),
                    cmd:   "nm-connection-editor".into(),
                },
            ]
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
                Widget::SectionHeader { label: "Profile".into() },
                Widget::InfoRow {
                    label: "Active".into(),
                    value: profile,
                },
                Widget::Button {
                    label: "Performance".into(),
                    cmd:   "powerprofilesctl set performance".into(),
                },
                Widget::Button {
                    label: "Balanced".into(),
                    cmd:   "powerprofilesctl set balanced".into(),
                },
                Widget::Button {
                    label: "Power Saver".into(),
                    cmd:   "powerprofilesctl set power-saver".into(),
                },
                Widget::SectionHeader { label: "Actions".into() },
                Widget::Button {
                    label: "Suspend".into(),
                    cmd:   "systemctl suspend".into(),
                },
                Widget::Button {
                    label: "Reboot".into(),
                    cmd:   "systemctl reboot".into(),
                },
                Widget::Button {
                    label: "Shutdown".into(),
                    cmd:   "systemctl poweroff".into(),
                },
            ]
        }

        Category::NixOS => {
            let hostname   = read_hostname();
            let generation = get_nix_generation();
            let kernel     = get_kernel_version();
            let uptime     = get_uptime();
            vec![
                Widget::SectionHeader { label: "System Info".into() },
                Widget::InfoRow { label: "Hostname".into(), value: hostname.clone() },
                Widget::InfoRow { label: "Kernel".into(),   value: kernel },
                Widget::InfoRow { label: "Uptime".into(),   value: uptime },
                Widget::SectionHeader { label: "NixOS".into() },
                Widget::InfoRow {
                    label: "Generation".into(),
                    value: generation,
                },
                Widget::Button {
                    label: "Flake Update".into(),
                    cmd:   "nix flake update /etc/nixos".into(),
                },
                Widget::Button {
                    label: "Rebuild Switch".into(),
                    cmd:   format!("sudo nixos-rebuild switch --flake /etc/nixos#{}", hostname),
                },
                Widget::Button {
                    label: "Rebuild Boot".into(),
                    cmd:   format!("sudo nixos-rebuild boot --flake /etc/nixos#{}", hostname),
                },
                Widget::Button {
                    label: "Garbage Collect".into(),
                    cmd:   "nix store gc".into(),
                },
                Widget::Button {
                    label: "Optimise Store".into(),
                    cmd:   "nix store optimise".into(),
                },
            ]
        }

        Category::System => {
            let hostname = read_hostname();
            let kernel   = get_kernel_version();
            let uptime   = get_uptime();
            let cpu      = get_cpu_model();
            let ram      = get_total_ram_gb();
            let disk_root = get_disk_usage("/");
            let disk_home = get_disk_usage("/home");
            vec![
                Widget::SectionHeader { label: "About This Machine".into() },
                Widget::InfoRow { label: "Hostname".into(),  value: hostname },
                Widget::InfoRow { label: "Kernel".into(),    value: kernel },
                Widget::InfoRow { label: "Uptime".into(),    value: uptime },
                Widget::SectionHeader { label: "Hardware".into() },
                Widget::InfoRow { label: "CPU".into(),       value: cpu },
                Widget::InfoRow { label: "Memory".into(),    value: ram },
                Widget::SectionHeader { label: "Storage".into() },
                Widget::InfoRow { label: "/".into(),         value: disk_root },
                Widget::InfoRow { label: "/home".into(),     value: disk_home },
            ]
        }
    }
}

pub fn apply_widget_action(widget: &mut Widget, new_value: f32) {
    match widget {
        Widget::Slider { value, cmd_template, .. } => {
            *value = new_value.clamp(0.0, 1.0);
            let pct = (*value * 100.0).round() as u32;
            let cmd = cmd_template.replace("{}", &pct.to_string());
            run_cmd(&cmd);
        }
        Widget::Toggle { value, cmd_on, cmd_off, .. } => {
            *value = new_value > 0.5;
            let cmd = if *value { *cmd_on } else { *cmd_off };
            run_cmd(cmd);
        }
        Widget::Button { cmd, .. } => {
            run_cmd(cmd);
        }
        Widget::InfoRow { .. } | Widget::SectionHeader { .. } => {}
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
    0
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

fn get_network_info() -> String {
    let out = std::process::Command::new("ip")
        .args(["-br", "addr"])
        .output()
        .ok();
    out.map(|o| {
        let text = String::from_utf8_lossy(&o.stdout).to_string();
        text.lines()
            .filter(|l| !l.starts_with("lo "))
            .take(2)
            .map(|l| l.split_whitespace().take(3).collect::<Vec<_>>().join(" "))
            .collect::<Vec<_>>()
            .join(", ")
    }).unwrap_or_else(|| "unavailable".into())
}

// keep for potential future use
#[allow(dead_code)]
fn get_network_info_legacy() -> String { get_network_info() }
