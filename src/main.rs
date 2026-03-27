mod config;
mod render;
mod modules;

use config::Config;
use modules::{
    clock::get_time_string,
    sysinfo::SysMonitor,
    windows::get_windows,
    workspaces::get_workspaces,
};
use render::Renderer;

fn main() {
    env_logger::init();
    log::info!("vitobar starting...");

    let config = Config::load();
    log::info!("config loaded: {:?}", config.colors.base00);

    let bar_h      = config.bar_height.unwrap_or(22);
    let taskbar_h  = config.taskbar_height.unwrap_or(22);
    let width      = 1920u32; // TODO: detect output resolution via Wayland

    // ── Initial render test (renders to PNG for debugging before Wayland is wired) ──
    let mut renderer = Renderer::new(width, bar_h);
    renderer.clear(&config.colors.base00);

    // Draw workspace blocks
    let workspaces = get_workspaces();
    let mut x = 4.0f32;
    for ws in &workspaces {
        let fill = if ws.is_active {
            &config.colors.base0d
        } else if ws.has_windows {
            &config.colors.base02
        } else {
            &config.colors.base01
        };

        renderer.draw_rect(x, 2.0, 18.0, 18.0, fill);
        renderer.draw_rect_outline(x, 2.0, 18.0, 18.0, &config.colors.base02, 1.5);
        x += 20.0;
    }

    // Draw status area placeholder
    renderer.draw_rect(width as f32 - 154.0, 2.0, 150.0, 18.0, &config.colors.base01);
    renderer.draw_rect_outline(width as f32 - 154.0, 2.0, 150.0, 18.0, &config.colors.base02, 1.5);

    // Save debug PNG
    renderer.pixmap.save_png("/tmp/vitobar_debug.png").ok();
    log::info!("debug render saved to /tmp/vitobar_debug.png");

    // ── Taskbar render ──
    let mut taskbar = Renderer::new(width, taskbar_h);
    taskbar.clear(&config.colors.base00);

    let windows = get_windows();
    let mut tx = 4.0f32;
    for win in &windows {
        let block_w = 130.0f32;
        taskbar.draw_rect(tx, 2.0, block_w, 18.0, &config.colors.base01);
        taskbar.draw_rect_outline(tx, 2.0, block_w, 18.0, &config.colors.base02, 1.5);

        // Workspace badge
        taskbar.draw_rect(tx + 3.0, 4.0, 13.0, 12.0, &config.colors.base00);
        taskbar.draw_rect_outline(tx + 3.0, 4.0, 13.0, 12.0, &config.colors.base05, 1.5);

        tx += block_w + 4.0;
    }

    taskbar.pixmap.save_png("/tmp/vitobar_taskbar_debug.png").ok();
    log::info!("taskbar debug render saved to /tmp/vitobar_taskbar_debug.png");

    // Print sys stats
    let mut monitor = SysMonitor::new();
    let stats = monitor.refresh();
    log::info!("cpu: {:.0}%  bat: {:?}  vol: {}%  brightness: {}%", stats.cpu_pct, stats.battery_pct, stats.volume_pct, stats.brightness_pct);
    log::info!("time: {}", get_time_string());

    log::info!("scaffold complete — Wayland layer shell integration next");
}
