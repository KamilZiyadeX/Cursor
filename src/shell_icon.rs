//! Uygulamanın simgesi değiştiğinde Windows Gezgini'ne haber verir.
//!
//! Gezgin simgeleri önbellekte (hem diskte hem bellekte) tutar ve aynı adlı bir
//! programın simgesi değişse bile eskisini göstermeye devam edebilir. LanChat
//! açılırken gömülü simgenin parmak izini kayıtlı olanla karşılaştırır; farklıysa
//! Gezgin'e bu dosyanın ve masaüstündeki LanChat kısayollarının simgesinin
//! değiştiğini bildirir.

use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;

const ICON: &[u8] = include_bytes!("../assets/icon/lanchat.ico");
const STAMP_FILE: &str = "icon_version";

/// Gömülü simgenin kısa parmak izi.
pub fn icon_fingerprint() -> String {
    Sha256::digest(ICON)[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Kayıtlı parmak izi `fp` değilse kaydeder ve `true` döndürür.
fn take_change(dir: &Path, fp: &str) -> bool {
    let path = dir.join(STAMP_FILE);
    if fs::read_to_string(&path).is_ok_and(|s| s.trim() == fp) {
        return false;
    }
    let _ = fs::create_dir_all(dir);
    let _ = fs::write(&path, fp);
    true
}

/// Simge, bu kullanıcıda en son görülenden farklıysa Gezgin'e haber verir.
pub fn refresh_if_changed(root: &Path) {
    if take_change(root, &icon_fingerprint()) {
        notify_shell();
    }
}

#[cfg(windows)]
fn notify_shell() {
    use std::os::windows::ffi::OsStrExt;
    use std::path::PathBuf;
    use windows_sys::Win32::UI::Shell::{
        SHCNE_ASSOCCHANGED, SHCNE_UPDATEITEM, SHCNF_FLUSH, SHCNF_IDLIST, SHCNF_PATHW,
        SHChangeNotify,
    };

    let mut paths: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        paths.push(exe);
    }
    // Masaüstündeki LanChat kısayolları (kişisel ve ortak masaüstü)
    let mut desktops: Vec<PathBuf> = Vec::new();
    if let Some(d) =
        directories::UserDirs::new().and_then(|u| u.desktop_dir().map(Path::to_path_buf))
    {
        desktops.push(d);
    }
    if let Ok(public) = std::env::var("PUBLIC") {
        desktops.push(PathBuf::from(public).join("Desktop"));
    }
    for dir in desktops {
        for entry in fs::read_dir(dir)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
        {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            if name.contains("lanchat") && (name.ends_with(".lnk") || name.ends_with(".exe")) {
                paths.push(entry.path());
            }
        }
    }

    for path in paths {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: `wide`, çağrı süresince yaşayan, sıfırla biten bir UTF-16 yoldur.
        unsafe {
            SHChangeNotify(
                SHCNE_UPDATEITEM as i32,
                SHCNF_PATHW | SHCNF_FLUSH,
                wide.as_ptr().cast(),
                std::ptr::null(),
            );
        }
    }
    // Genel "simgeleri yenile" bildirimi (ie4uinit -show'un yaptığına benzer)
    // SAFETY: bu olay için öğe parametreleri boş olmalıdır.
    unsafe {
        SHChangeNotify(
            SHCNE_ASSOCCHANGED as i32,
            SHCNF_IDLIST | SHCNF_FLUSH,
            std::ptr::null(),
            std::ptr::null(),
        );
    }
}

#[cfg(not(windows))]
fn notify_shell() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notifies_only_when_icon_changes() {
        let dir = std::env::temp_dir().join(format!("lanchat-icon-{}", uuid::Uuid::new_v4()));
        let fp = icon_fingerprint();
        assert_eq!(fp.len(), 16);
        assert!(take_change(&dir, &fp), "ilk açılışta bildirmeli");
        assert!(!take_change(&dir, &fp), "simge aynıysa bildirmemeli");
        assert!(
            take_change(&dir, "baska-simge"),
            "simge değişince bildirmeli"
        );
        assert!(take_change(&dir, &fp));
        fs::remove_dir_all(dir).unwrap();
    }
}
