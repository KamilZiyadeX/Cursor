// Windows'ta sürüm derlemesinde arka planda konsol penceresi açılmasın
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod net;
mod protocol;
mod root;
mod secure;
mod storage;

use eframe::egui;

fn main() -> eframe::Result {
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

/// Basit bir uygulama simgesi: mavi zemin üzerinde beyaz konuşma balonu.
fn app_icon() -> egui::IconData {
    const S: usize = 64;
    let mut rgba = vec![0u8; S * S * 4];
    for y in 0..S {
        for x in 0..S {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            // Yuvarlatılmış kare
            let r = 14.0;
            let cx = fx.clamp(r, S as f32 - r);
            let cy = fy.clamp(r, S as f32 - r);
            let inside_bg = (fx - cx).powi(2) + (fy - cy).powi(2) <= r * r;
            // Balon gövdesi ve kuyruğu
            let in_body = (12.0..52.0).contains(&fx) && (16.0..42.0).contains(&fy);
            let in_tail = (42.0..52.0).contains(&fy) && fx >= 20.0 && fx < 20.0 + (52.0 - fy);
            let dot = [23.0f32, 32.0, 41.0]
                .iter()
                .any(|dx| (fx - dx).powi(2) + (fy - 29.0).powi(2) <= 9.0);
            let px = &mut rgba[(y * S + x) * 4..][..4];
            if !inside_bg {
                continue;
            }
            let c = if (in_body || in_tail) && !dot {
                [255, 255, 255, 255]
            } else {
                [37, 99, 235, 255]
            };
            px.copy_from_slice(&c);
        }
    }
    egui::IconData {
        rgba,
        width: S as u32,
        height: S as u32,
    }
}
