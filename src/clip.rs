//! Pano (kopyala-yapıştır) ile gönderme: kopyalanmış dosyalar, ekran görüntüsü
//! ya da metin.

use eframe::egui::ColorImage;
use std::io;
use std::path::{Path, PathBuf};

/// Panoda ne var?
pub enum ClipContent {
    /// Dosya Gezgini'nde kopyalanmış dosya ve klasörler
    Files(Vec<PathBuf>),
    /// Resim (ör. Win+Shift+S ile alınan ekran görüntüsü)
    Image(ColorImage),
    Text(String),
    Empty,
}

/// Panonun içeriğini okur (önce dosyalar, sonra resim, sonra metin).
pub fn read() -> Result<ClipContent, String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| format!("Panoya erişilemedi: {e}"))?;
    if let Ok(files) = cb.get().file_list()
        && !files.is_empty()
    {
        return Ok(ClipContent::Files(files));
    }
    if let Ok(img) = cb.get_image() {
        let size = [img.width, img.height];
        return Ok(ClipContent::Image(ColorImage::from_rgba_unmultiplied(
            size, &img.bytes,
        )));
    }
    match cb.get_text() {
        Ok(t) if !t.trim().is_empty() => Ok(ClipContent::Text(t.replace("\r\n", "\n"))),
        _ => Ok(ClipContent::Empty),
    }
}

/// Resmi `dir` içine "Ekran görüntüsü <tarih saat>.png" olarak kaydeder.
pub fn save_png(img: &ColorImage, dir: &Path) -> io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let stamp = chrono::Local::now().format("%Y-%m-%d %H.%M.%S");
    let mut path = dir.join(format!("Ekran görüntüsü {stamp}.png"));
    let mut n = 1;
    while path.exists() {
        n += 1;
        path = dir.join(format!("Ekran görüntüsü {stamp} ({n}).png"));
    }
    let [w, h] = img.size;
    let bytes: Vec<u8> = img
        .pixels
        .iter()
        .flat_map(|c| c.to_srgba_unmultiplied())
        .collect();
    let buf = image::RgbaImage::from_raw(w as u32, h as u32, bytes)
        .ok_or_else(|| io::Error::other("Resim verisi bozuk"))?;
    buf.save_with_format(&path, image::ImageFormat::Png)
        .map_err(io::Error::other)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Color32;

    #[test]
    fn screenshot_is_saved_as_png() {
        let dir = std::env::temp_dir().join(format!("lanchat-clip-{}", uuid::Uuid::new_v4()));
        let mut img = ColorImage::filled([3, 2], Color32::from_rgb(200, 30, 30));
        img.pixels[1] = Color32::WHITE;
        let p1 = save_png(&img, &dir).unwrap();
        let p2 = save_png(&img, &dir).unwrap();
        assert_ne!(
            p1, p2,
            "aynı saniyede kaydedilen ikinci resim üzerine yazmamalı"
        );
        let back = image::open(&p1).unwrap().to_rgba8();
        assert_eq!(back.dimensions(), (3, 2));
        assert_eq!(back.get_pixel(0, 0).0, [200, 30, 30, 255]);
        assert_eq!(back.get_pixel(1, 0).0, [255, 255, 255, 255]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
