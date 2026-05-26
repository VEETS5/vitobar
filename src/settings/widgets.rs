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
    Selector {
        label:    String,
        options:  Vec<SelectorOption>,
        selected: usize, // index into options; out-of-range = none highlighted
        key:      &'static str,
    },
}

#[derive(Debug, Clone)]
pub struct SelectorOption {
    pub label:    String,
    pub value:    String,
    pub swatches: Option<Vec<String>>, // 16 hex colors for theme previews
}

#[derive(Debug)]
pub enum WidgetResult {
    None,
    ConfigUpdate { key: &'static str, value: f32 },
    ConfigUpdateStr { key: &'static str, value: String },
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
    let secs = std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|s| s.split_whitespace().next().map(str::to_string))
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0) as u64;
    let d = secs / 86400;
    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    if d > 0 {
        format!("{}d {}h {}m", d, h, m)
    } else if h > 0 {
        format!("{}h {}m", h, m)
    } else {
        format!("{}m", m)
    }
}

fn get_os_name() -> String {
    std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("PRETTY_NAME="))
                .map(|l| l.trim_start_matches("PRETTY_NAME=").trim_matches('"').to_string())
        })
        .unwrap_or_else(|| "Linux".into())
}

fn get_nixos_version() -> String {
    std::process::Command::new("nixos-version")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

fn get_wm() -> String {
    let ver = std::process::Command::new("niri")
        .arg("--version")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());
    match ver {
        Some(v) => v,
        None => "niri".into(),
    }
}

fn get_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .map(|s| s.rsplit('/').next().unwrap_or(&s).to_string())
        .unwrap_or_else(|| "unknown".into())
}

fn get_gpu_model() -> String {
    let out = std::process::Command::new("lspci").output().ok();
    out.and_then(|o| {
        let text = String::from_utf8_lossy(&o.stdout).to_string();
        text.lines()
            .find(|l| l.contains("VGA compatible controller") || l.contains("3D controller"))
            .and_then(|l| l.splitn(2, ':').nth(1))
            .and_then(|rest| rest.splitn(2, ':').nth(1))
            .map(|s| s.trim().to_string())
    })
    .filter(|s| !s.is_empty())
    .unwrap_or_else(|| "unknown".into())
}

fn get_ram_usage() -> String {
    let content = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let read_kb = |key: &str| -> u64 {
        content.lines()
            .find(|l| l.starts_with(key))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0)
    };
    let total = read_kb("MemTotal:");
    let avail = read_kb("MemAvailable:");
    let used = total.saturating_sub(avail);
    let gb = |kb: u64| kb as f64 / 1_048_576.0;
    format!("{:.1} / {:.1} GB", gb(used), gb(total))
}

fn get_cpu_model() -> String {
    let content = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    content.lines()
        .find(|l| l.starts_with("model name"))
        .and_then(|l| l.splitn(2, ':').nth(1))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".into())
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

/// Active power profile, or None when power-profiles-daemon is unavailable.
fn get_power_profile() -> Option<String> {
    let out = std::process::Command::new("powerprofilesctl").arg("get").output().ok()?;
    if !out.status.success() { return None; }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
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
    // Use --rescan no to avoid triggering a WiFi scan that blocks the UI thread
    let out = std::process::Command::new("nmcli")
        .args(["-t", "-f", "active,ssid", "dev", "wifi", "list", "--rescan", "no"])
        .output().ok();
    out.and_then(|o| {
        if !o.status.success() { return None; }
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
    let out = std::process::Command::new("timeout")
        .args(["2", "bluetoothctl", "paired-devices"]).output().ok();
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

/// True if `cmd` is found on PATH.
fn command_exists(cmd: &str) -> bool {
    std::process::Command::new("sh")
        .args(["-c", &format!("command -v {} >/dev/null 2>&1", cmd)])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// True if a Bluetooth controller is present on the system.
fn bt_adapter_present() -> bool {
    if let Ok(rd) = std::fs::read_dir("/sys/class/bluetooth") {
        if rd.flatten().next().is_some() {
            return true;
        }
    }
    std::process::Command::new("timeout")
        .args(["2", "bluetoothctl", "list"])
        .output()
        .map(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty())
        .unwrap_or(false)
}

/// First installed Bluetooth manager GUI, if any.
fn bt_manager() -> Option<&'static str> {
    ["blueman-manager", "overskride", "blueberry"]
        .into_iter()
        .find(|m| command_exists(m))
}

/// Build the wlsunset invocation for the current night-light config. The
/// command kills any running instance first so it can be re-applied live.
fn build_nightlight_cmd(config: &crate::config::Config) -> String {
    let temp = config.nightlight_temp();
    if config.nightlight_mode() == "auto" {
        if let (Some(lat), Some(lon)) = (config.latitude, config.longitude) {
            return format!(
                "pkill wlsunset; wlsunset -l {:.4} -L {:.4} -t {} -T 6500",
                lat, lon, temp
            );
        }
    }
    // Scheduled (also the fallback when Auto has no location): -s is sunset
    // (night begins), -S is sunrise (night ends).
    let start = config.nightlight_start();
    let end = config.nightlight_end();
    format!(
        "pkill wlsunset; wlsunset -S {:02}:00 -s {:02}:00 -t {} -T 6500",
        end, start, temp
    )
}

pub fn build_widgets(cat: Category, config: &crate::config::Config) -> Vec<Widget> {
    match cat {
        Category::Appearance => {
            let bar_h     = config.bar_h() as f32;
            let taskbar_h = config.taskbar_h() as f32;
            let font_sz   = config.font_size.unwrap_or(11.0);
            let opacity_pct = config.opacity() * 100.0;

            // ── Theme picker ──────────────────────────────────────────────
            // First option follows Stylix (empty value = clear the override).
            let themes = crate::config::available_themes();
            let mut theme_opts: Vec<SelectorOption> = vec![SelectorOption {
                label:    "Stylix".into(),
                value:    String::new(),
                swatches: crate::config::stylix_colors().map(|c| c.swatches()),
            }];
            theme_opts.extend(themes.iter().map(|t| SelectorOption {
                label:    t.name.clone(),
                value:    t.name.clone(),
                swatches: Some(t.colors.swatches()),
            }));
            // index 0 = Stylix; real themes are offset by 1.
            let theme_sel = match config.selected_theme.as_deref() {
                Some(name) if !name.is_empty() => {
                    themes.iter().position(|t| t.name == name).map(|i| i + 1).unwrap_or(0)
                }
                _ => 0,
            };

            // ── Density presets (mini/compact/default/comfortable/spacious) ─
            let densities: &[(&str, u32)] = &[
                ("Mini", 21), ("Compact", 25), ("Default", 31),
                ("Comfortable", 37), ("Spacious", 47),
            ];
            let density_opts: Vec<SelectorOption> = densities.iter().map(|(label, h)| SelectorOption {
                label:    (*label).into(),
                value:    h.to_string(),
                swatches: None,
            }).collect();
            let density_sel = densities.iter()
                .position(|(_, h)| *h == bar_h as u32)
                .unwrap_or(usize::MAX);

            vec![
                Widget::SectionHeader { label: "Theme".into() },
                Widget::Selector {
                    label:    "Color Scheme".into(),
                    options:  theme_opts,
                    selected: theme_sel,
                    key:      "selected_theme",
                },
                Widget::InfoRow {
                    label: "".into(),
                    value: "Stylix follows your system colors. Restart the bar to apply.".into(),
                },

                Widget::SectionHeader { label: "Bar".into() },
                Widget::Selector {
                    label:    "Density".into(),
                    options:  density_opts,
                    selected: density_sel,
                    key:      "bar_density",
                },
                Widget::ConfigSlider {
                    label: "Bar Height".into(),
                    value: ((bar_h - 16.0) / 32.0).clamp(0.0, 1.0),  // 16..48
                    min: 16.0,
                    max: 48.0,
                    key: "bar_height",
                },
                Widget::ConfigSlider {
                    label: "Taskbar Height".into(),
                    value: ((taskbar_h - 16.0) / 32.0).clamp(0.0, 1.0),
                    min: 16.0,
                    max: 48.0,
                    key: "taskbar_height",
                },
                Widget::ConfigSlider {
                    label: "Opacity".into(),
                    value: ((opacity_pct - 50.0) / 50.0).clamp(0.0, 1.0),  // 50..100
                    min: 50.0,
                    max: 100.0,
                    key: "bar_opacity",
                },

                Widget::SectionHeader { label: "Typography".into() },
                Widget::ConfigSlider {
                    label: "Font Size".into(),
                    value: ((font_sz - 8.0) / 12.0).clamp(0.0, 1.0),  // 8..20
                    min: 8.0,
                    max: 20.0,
                    key: "font_size",
                },

                Widget::SectionHeader { label: "Config".into() },
                Widget::InfoRow {
                    label: "File".into(),
                    value: crate::config::config_path().to_string_lossy().to_string(),
                },
            ]
        }

        Category::Display => {
            let mode = config.nightlight_mode();
            let temp = config.nightlight_temp();
            let mode_opts = vec![
                SelectorOption { label: "Scheduled".into(),      value: "scheduled".into(), swatches: None },
                SelectorOption { label: "Auto (location)".into(), value: "auto".into(),      swatches: None },
            ];
            let mode_sel = if mode == "auto" { 1 } else { 0 };

            let mut widgets = vec![
                Widget::SectionHeader { label: "Night Light".into() },
                Widget::InfoRow {
                    label: "".into(),
                    value: "Warms screen colors to reduce blue light (needs wlsunset).".into(),
                },
                Widget::Selector {
                    label:    "Mode".into(),
                    options:  mode_opts,
                    selected: mode_sel,
                    key:      "nightlight_mode",
                },
                Widget::ConfigSlider {
                    label: "Temperature (K)".into(),
                    value: ((temp as f32 - 2500.0) / 3500.0).clamp(0.0, 1.0),  // 2500..6000, lower = warmer
                    min: 2500.0,
                    max: 6000.0,
                    key: "nightlight_temp",
                },
            ];
            if mode == "auto" {
                let loc = match (config.latitude, config.longitude) {
                    (Some(la), Some(lo)) => format!("{:.3}, {:.3}", la, lo),
                    _ => "Set latitude/longitude in config.toml".into(),
                };
                widgets.push(Widget::InfoRow { label: "Location".into(), value: loc });
            } else {
                widgets.push(Widget::ConfigSlider {
                    label: "Night starts (h)".into(),
                    value: (config.nightlight_start() as f32 / 23.0).clamp(0.0, 1.0),
                    min: 0.0, max: 23.0, key: "nightlight_start",
                });
                widgets.push(Widget::ConfigSlider {
                    label: "Night ends (h)".into(),
                    value: (config.nightlight_end() as f32 / 23.0).clamp(0.0, 1.0),
                    min: 0.0, max: 23.0, key: "nightlight_end",
                });
            }
            widgets.push(Widget::Button {
                label: "\u{f185}  Apply Night Light".into(),
                cmd:   build_nightlight_cmd(config),
            });
            widgets.push(Widget::Button {
                label: "\u{f186}  Turn Off".into(),
                cmd:   "pkill wlsunset".into(),
            });
            widgets
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
            if !bt_adapter_present() {
                return vec![
                    Widget::SectionHeader { label: "Bluetooth".into() },
                    Widget::InfoRow { label: "Status".into(), value: "No adapter found".into() },
                ];
            }
            let bt = get_bt_info();
            let mut widgets = vec![
                Widget::SectionHeader { label: "Bluetooth".into() },
                Widget::Toggle {
                    label:   "Power".into(),
                    value:   bt.powered,
                    cmd_on:  "rfkill unblock bluetooth; bluetoothctl power on",
                    cmd_off: "bluetoothctl power off",
                },
                Widget::InfoRow {
                    label: "Status".into(),
                    value: bt.status_text,
                },
            ];
            match bt_manager() {
                Some(mgr) => widgets.push(Widget::Button {
                    label: "\u{f294}  Open Bluetooth Manager".into(),
                    cmd:   mgr.into(),
                }),
                None => widgets.push(Widget::InfoRow {
                    label: "Manager".into(),
                    value: "Install blueman, overskride, or blueberry".into(),
                }),
            }
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

        Category::Power => {
            let mut widgets = vec![];

            let bat = get_battery_status();
            if bat != "No battery" {
                widgets.push(Widget::SectionHeader { label: "Battery".into() });
                widgets.push(Widget::InfoRow { label: "Status".into(), value: bat });
            }

            widgets.push(Widget::SectionHeader { label: "Power Profile".into() });
            match get_power_profile() {
                Some(active) => {
                    let opts = vec![
                        SelectorOption { label: "Power Saver".into(), value: "power-saver".into(), swatches: None },
                        SelectorOption { label: "Balanced".into(),    value: "balanced".into(),    swatches: None },
                        SelectorOption { label: "Performance".into(), value: "performance".into(), swatches: None },
                    ];
                    let sel = opts.iter().position(|o| o.value == active).unwrap_or(usize::MAX);
                    widgets.push(Widget::Selector {
                        label:    "Profile".into(),
                        options:  opts,
                        selected: sel,
                        key:      "power_profile",
                    });
                }
                None => widgets.push(Widget::InfoRow {
                    label: "Profile".into(),
                    value: "power-profiles-daemon not available".into(),
                }),
            }

            widgets.push(Widget::SectionHeader { label: "Session".into() });
            widgets.push(Widget::Button { label: "\u{f186}  Suspend".into(),   cmd: "systemctl suspend".into() });
            widgets.push(Widget::Button { label: "\u{f7c9}  Hibernate".into(), cmd: "systemctl hibernate".into() });
            widgets.push(Widget::Button { label: "\u{f021}  Reboot".into(),    cmd: "systemctl reboot".into() });
            widgets.push(Widget::Button { label: "\u{f011}  Shutdown".into(),  cmd: "systemctl poweroff".into() });
            widgets
        }

        Category::System => {
            vec![
                Widget::SectionHeader { label: "System".into() },
                Widget::InfoRow { label: "OS".into(),        value: get_os_name() },
                Widget::InfoRow { label: "Hostname".into(),  value: read_hostname() },
                Widget::InfoRow { label: "Kernel".into(),    value: get_kernel_version() },
                Widget::InfoRow { label: "WM".into(),        value: get_wm() },
                Widget::InfoRow { label: "Shell".into(),     value: get_shell() },
                Widget::InfoRow { label: "Uptime".into(),    value: get_uptime() },
                Widget::SectionHeader { label: "Hardware".into() },
                Widget::InfoRow { label: "CPU".into(),       value: get_cpu_model() },
                Widget::InfoRow { label: "GPU".into(),       value: get_gpu_model() },
                Widget::InfoRow { label: "Memory".into(),    value: get_ram_usage() },
                Widget::SectionHeader { label: "Storage".into() },
                Widget::InfoRow { label: "/".into(),         value: get_disk_usage("/") },
                Widget::InfoRow { label: "/home".into(),     value: get_disk_usage("/home") },
                Widget::SectionHeader { label: "NixOS".into() },
                Widget::InfoRow { label: "Version".into(),    value: get_nixos_version() },
                Widget::InfoRow { label: "Generation".into(), value: get_nix_generation() },
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
        Widget::InfoRow { .. } | Widget::SectionHeader { .. } | Widget::Selector { .. } => {
            WidgetResult::None
        }
    }
}

/// Select option `idx` of a Selector widget, returning the new string value.
pub fn apply_selector(widget: &mut Widget, idx: usize) -> WidgetResult {
    if let Widget::Selector { options, selected, key, .. } = widget {
        if idx < options.len() {
            *selected = idx;
            return WidgetResult::ConfigUpdateStr { key, value: options[idx].value.clone() };
        }
    }
    WidgetResult::None
}

fn run_cmd(cmd: &str) {
    // Use sh -c to support compound commands (&&, ||, pipes, etc.)
    std::process::Command::new("sh")
        .args(["-c", cmd])
        .spawn().ok();
}

// ── System state readers ──────────────────────────────────────────────────────

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
    let show = std::process::Command::new("timeout")
        .args(["2", "bluetoothctl", "show"]).output().ok();
    let powered = show.as_ref()
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).contains("Powered: yes"))
        .unwrap_or(false);

    if !powered {
        return BtInfo { powered: false, status_text: "Off".into() };
    }

    let info = std::process::Command::new("timeout")
        .args(["2", "bluetoothctl", "info"]).output().ok();
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
