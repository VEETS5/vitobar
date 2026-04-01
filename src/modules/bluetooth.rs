#[derive(Debug, Clone, PartialEq)]
pub enum BluetoothStatus {
    Off,
    OnNoDevice,
    Connected { device_name: String },
}

#[derive(Debug, Clone)]
pub struct BluetoothState {
    pub status: BluetoothStatus,
}

impl Default for BluetoothState {
    fn default() -> Self {
        Self { status: BluetoothStatus::Off }
    }
}

pub fn get_bluetooth() -> BluetoothState {
    // Use timeout to prevent blocking the bar if bluetooth service is unresponsive
    let powered = std::process::Command::new("timeout")
        .args(["2", "bluetoothctl", "show"])
        .output()
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).contains("Powered: yes"))
        .unwrap_or(false);

    if !powered {
        return BluetoothState { status: BluetoothStatus::Off };
    }

    let info_out = std::process::Command::new("timeout")
        .args(["2", "bluetoothctl", "info"])
        .output()
        .ok();

    if let Some(out) = info_out {
        let text = String::from_utf8_lossy(&out.stdout);
        if text.contains("Connected: yes") {
            let name = text
                .lines()
                .find(|l| l.trim_start().starts_with("Name:"))
                .and_then(|l| l.splitn(2, ':').nth(1))
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|| "device".into());
            return BluetoothState {
                status: BluetoothStatus::Connected { device_name: name },
            };
        }
    }

    BluetoothState { status: BluetoothStatus::OnNoDevice }
}
