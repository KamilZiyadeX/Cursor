// Windows'ta sürüm derlemesinde arka planda konsol penceresi açılmasın
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod net;
mod protocol;
mod root;
mod secure;
mod shell_icon;
mod storage;
mod vault;

use eframe::egui;

fn main() -> eframe::Result {
    // Simge bu sürümde değiştiyse Windows Gezgini'ne haber ver (arka planda)
    std::thread::spawn(|| shell_icon::refresh_if_changed(&storage::root_dir()));

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("LanChat")
            .with_app_id("lanchat")
            .with_inner_size([960.0, 640.0])
            .with_min_inner_size([560.0, 380.0])
            .with_drag_and_drop(true)
            .with_icon(app_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "LanChat",
        options,
        Box::new(|cc| Ok(Box::new(root::Root::new(cc)))),
    )
}

/// Pencere ve görev çubuğu simgesi (assets/icon altındaki logo).
fn app_icon() -> egui::IconData {
    eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon/lanchat-256.png"))
        .expect("gömülü logo geçerli bir PNG")
}
