//! Ağdan indirilen yeni sürümü kurma.
//!
//! Windows çalışan bir programın silinmesine izin vermez, ama adının
//! değiştirilmesine izin verir. Bu yüzden çalışan program `<ad>.old` olarak
//! yeniden adlandırılır, yenisi onun yerine konur ve program yeniden başlatılır.
//! Eski dosya bir sonraki açılışta silinir.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// İndirilen güncellemenin yazılacağı yer (programın yanında).
pub fn download_path(exe: &Path) -> PathBuf {
    exe.with_file_name("lanchat-guncelleme.indirilen")
}

fn old_path(exe: &Path) -> PathBuf {
    let mut name = exe.file_name().unwrap_or_default().to_owned();
    name.push(".old");
    exe.with_file_name(name)
}

/// `exe` dosyasını `downloaded` ile değiştirir; eskisi `.old` olarak kalır.
pub fn install(exe: &Path, downloaded: &Path) -> io::Result<()> {
    let old = old_path(exe);
    let _ = fs::remove_file(&old);
    fs::rename(exe, &old)?;
    if let Err(e) = fs::rename(downloaded, exe) {
        // Geri al: eski program yerinde kalsın
        let _ = fs::rename(&old, exe);
        return Err(e);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(exe, fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// Önceki güncellemeden kalan eski programı siler.
pub fn cleanup_old() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = fs::remove_file(old_path(&exe));
    }
}

/// Yarım kalmış ya da kurulmamış eski bir indirmeyi siler. Başka bir hesap
/// penceresi o sırada indiriyor olabileceğinden yalnızca bir saatten eski
/// dosyalara dokunur.
pub fn cleanup_download() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let dl = download_path(&exe);
    let mut part = dl.clone().into_os_string();
    part.push(".part");
    for f in [dl, PathBuf::from(part)] {
        let stale = fs::metadata(&f)
            .and_then(|m| m.modified())
            .is_ok_and(|t| t.elapsed().is_ok_and(|e| e.as_secs() > 3600));
        if stale {
            let _ = fs::remove_file(&f);
        }
    }
}

/// Yeni programı aynı hesapla başlatır.
pub fn restart(exe: &Path, profile_id: &str) -> io::Result<()> {
    std::process::Command::new(exe)
        .arg("--hesap-id")
        .arg(profile_id)
        .arg("--guncelleme")
        .spawn()
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_replaces_program_and_keeps_old() {
        let d = std::env::temp_dir().join(format!("lanchat-upd-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&d).unwrap();
        let exe = d.join("LanChat.exe");
        fs::write(&exe, b"eski surum").unwrap();
        let new = download_path(&exe);
        fs::write(&new, b"yeni surum").unwrap();

        install(&exe, &new).unwrap();
        assert_eq!(fs::read(&exe).unwrap(), b"yeni surum");
        assert_eq!(fs::read(old_path(&exe)).unwrap(), b"eski surum");
        assert!(!new.exists());

        // İndirilen dosya yoksa kurulum geri alınır, program yerinde kalır
        assert!(install(&exe, &d.join("yok")).is_err());
        assert_eq!(fs::read(&exe).unwrap(), b"yeni surum");
        fs::remove_dir_all(d).unwrap();
    }
}
