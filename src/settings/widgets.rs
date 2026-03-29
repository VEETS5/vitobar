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

pub fn build_widgets(cat: Category) -> Vec<Widget> {
    match cat {
        Category::Display => {
            let brightness = get_brightness_pct();
            vec![
                Widget::Slider {
                    label: "Brightness".into(),
                    value: brightness as f32 / 100.0,
                    cmd_template: "brightnessctl set {}%",
                },
            ]
        }

        Category::Audio => {
            let volume = get_volume_pct();
            let muted  = get_muted();
            vec![
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
            ]
        }

        Category::Bluetooth => {
            let bt = get_bt_info();
            vec![
                Widget::Toggle {
                    label:   "Bluetooth".into(),
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
            ]
        }

        Category::Network => {
            let iface = get_network_info();
            vec![
                Widget::InfoRow {
                    label: "Interface".into(),
                    value: iface,
                },
                Widget::Button {
                    label: "Open Network Manager".into(),
                    cmd:   "nm-connection-editor".into(),
                },
            ]
        }

        Category::Power => vec![
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
        ],

        Category::NixOS => {
            let hostname = read_hostname();
            let generation = get_nix_generation();
            vec![
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
        Widget::InfoRow { .. } => {}
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
