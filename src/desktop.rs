//! Masaüstü entegrasyonu: sistem tepsisi simgesi, Windows açılınca başlama ve
//! Windows bildirimleri. Bu özellikler yalnızca Windows'ta vardır; diğer
//! sistemlerde işlevler hiçbir şey yapmaz.

use std::sync::mpsc::Sender;

/// Tepsi simgesinden ya da bildirimden uygulamaya gelen istekler
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub enum DesktopEvent {
    /// Pencereyi göster (isteğe bağlı olarak bu kişinin sohbetini aç)
    Show(Option<String>),
    /// Programı tamamen kapat
    Quit,
}

/// Bu platformda sistem tepsisi var mı?
pub const HAS_TRAY: bool = cfg!(windows);

/// Bildirimler için kayıtlı uygulama kimliği (AppUserModelID)
#[cfg(windows)]
const AUMID: &str = "Kapo.LanChat";

/// Windows "Başlangıç" kaydındaki değer adı (hesap başına bir tane)
#[cfg_attr(not(windows), allow(dead_code))]
fn autostart_value_name(profile_id: &str) -> String {
    format!("LanChat-{profile_id}")
}

/// Başlangıçta çalıştırılacak komut
#[cfg_attr(not(windows), allow(dead_code))]
fn autostart_command(exe: &std::path::Path, profile_id: &str) -> String {
    format!("\"{}\" --hesap-id {profile_id} --arka-plan", exe.display())
}

/* ------------------------------ Windows ------------------------------ */

#[cfg(windows)]
mod imp {
    use super::*;
    use std::ffi::OsStr;
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::sync::Mutex;
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
        RegCloseKey, RegCreateKeyExW, RegDeleteKeyValueW, RegGetValueW, RegSetValueExW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSMICON};

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

    fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
        s.as_ref().encode_wide().chain(Some(0)).collect()
    }

    fn check(code: u32) -> io::Result<()> {
        if code == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(code as i32))
        }
    }

    /// HKCU altındaki `subkey` anahtarına (yoksa oluşturarak) metin değerleri yazar.
    fn write_strings(subkey: &str, values: &[(&str, &str)]) -> io::Result<()> {
        let mut key: HKEY = std::ptr::null_mut();
        // SAFETY: tüm işaretçiler çağrı süresince geçerli, sıfırla biten UTF-16 dizilerdir.
        unsafe {
            check(RegCreateKeyExW(
                HKEY_CURRENT_USER,
                wide(subkey).as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                std::ptr::null(),
                &mut key,
                std::ptr::null_mut(),
            ))?;
            let mut result = Ok(());
            for (name, value) in values {
                let data = wide(value);
                result = check(RegSetValueExW(
                    key,
                    wide(name).as_ptr(),
                    0,
                    REG_SZ,
                    data.as_ptr().cast(),
                    (data.len() * 2) as u32,
                ));
                if result.is_err() {
                    break;
                }
            }
            RegCloseKey(key);
            result
        }
    }

    fn read_string(subkey: &str, name: &str) -> Option<String> {
        let mut buf = vec![0u16; 2048];
        let mut size = (buf.len() * 2) as u32;
        // SAFETY: arabellek ve boyutu doğru verildi; işaretçiler çağrı boyunca geçerli.
        let code = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                wide(subkey).as_ptr(),
                wide(name).as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buf.as_mut_ptr().cast(),
                &mut size,
            )
        };
        if code != ERROR_SUCCESS {
            return None;
        }
        let len = (size as usize / 2).saturating_sub(1);
        buf.truncate(len);
        Some(String::from_utf16_lossy(&buf))
    }

    pub fn autostart_enabled(profile_id: &str) -> bool {
        read_string(RUN_KEY, &autostart_value_name(profile_id)).is_some()
    }

    pub fn set_autostart(profile_id: &str, enabled: bool) -> io::Result<()> {
        let name = autostart_value_name(profile_id);
        if enabled {
            let exe = std::env::current_exe()?;
            write_strings(RUN_KEY, &[(&name, &autostart_command(&exe, profile_id))])
        } else {
            // SAFETY: işaretçiler çağrı boyunca geçerli, sıfırla biten UTF-16 dizileri.
            let code = unsafe {
                RegDeleteKeyValueW(
                    HKEY_CURRENT_USER,
                    wide(RUN_KEY).as_ptr(),
                    wide(&name).as_ptr(),
                )
            };
            // Zaten yoksa sorun değil
            if code == ERROR_SUCCESS || code == 2 {
                Ok(())
            } else {
                check(code)
            }
        }
    }

    /// Program taşındıysa başlangıç kaydındaki yolu günceller.
    pub fn refresh_autostart(profile_id: &str) {
        let name = autostart_value_name(profile_id);
        if let (Some(current), Ok(exe)) = (read_string(RUN_KEY, &name), std::env::current_exe()) {
            let want = autostart_command(&exe, profile_id);
            if current != want {
                let _ = write_strings(RUN_KEY, &[(&name, &want)]);
            }
        }
    }

    /* ---------------- Bildirimler ---------------- */

    static APP_ID: Mutex<Option<String>> = Mutex::new(None);

    /// Bildirimlerde "LanChat" adı ve logosu görünsün diye uygulama kimliğini kaydeder.
    fn app_id(icon_dir: &Path) -> String {
        let mut guard = APP_ID.lock().unwrap();
        if let Some(id) = guard.as_ref() {
            return id.clone();
        }
        let icon = icon_dir.join("lanchat-bildirim.png");
        let _ = std::fs::create_dir_all(icon_dir);
        let _ = std::fs::write(
            &icon,
            include_bytes!("../assets/icon/lanchat-256.png").as_slice(),
        );
        let key = format!(r"Software\Classes\AppUserModelId\{AUMID}");
        let icon_s = icon.display().to_string();
        let id = match write_strings(&key, &[("DisplayName", "LanChat"), ("IconUri", &icon_s)]) {
            Ok(()) => AUMID.to_string(),
            Err(_) => tauri_winrt_notification::Toast::POWERSHELL_APP_ID.to_string(),
        };
        *guard = Some(id.clone());
        id
    }

    pub fn notify(
        icon_dir: &Path,
        title: &str,
        body: &str,
        peer_id: Option<String>,
        events: Sender<DesktopEvent>,
        wake: impl Fn() + Send + Sync + Clone + 'static,
    ) {
        use tauri_winrt_notification::{Sound, Toast};
        let id = app_id(icon_dir);
        let (title, body) = (title.to_string(), body.to_string());
        // WinRT çağrıları kısa da olsa bekletebilir: arayüzü durdurmasın
        std::thread::spawn(move || {
            let make = |app: &str| {
                let events = events.clone();
                let peer_id = peer_id.clone();
                let wake = wake.clone();
                Toast::new(app)
                    .title(&title)
                    .text1(&body)
                    .sound(Some(Sound::Default))
                    .on_activated(move |_| {
                        let _ = events.send(DesktopEvent::Show(peer_id.clone()));
                        wake();
                        Ok(())
                    })
            };
            let shown = make(&id).show();
            if shown.is_err() && id != Toast::POWERSHELL_APP_ID {
                let _ = make(Toast::POWERSHELL_APP_ID).show();
            }
        });
    }

    /* ---------------- Tepsi ---------------- */

    pub struct Tray {
        icon: TrayIcon,
    }

    impl Tray {
        pub fn new(
            tooltip: &str,
            events: Sender<DesktopEvent>,
            wake: impl Fn() + Send + Sync + Clone + 'static,
        ) -> Option<Tray> {
            let show = MenuItem::new("LanChat'i aç", true, None);
            let quit = MenuItem::new("Çıkış", true, None);
            let menu = Menu::new();
            menu.append_items(&[&show, &PredefinedMenuItem::separator(), &quit])
                .ok()?;
            // SAFETY: GetSystemMetrics'in yan etkisi yoktur.
            let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16) as u32;
            let icon = tray_icon::Icon::from_resource(1, Some((size, size))).ok()?;
            let tray = TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_menu_on_left_click(false)
                .with_tooltip(tooltip)
                .with_icon(icon)
                .build()
                .ok()?;

            let (show_id, quit_id) = (show.id().clone(), quit.id().clone());
            {
                let events = events.clone();
                let wake = wake.clone();
                TrayIconEvent::set_event_handler(Some(move |ev: TrayIconEvent| {
                    let open = matches!(
                        ev,
                        TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        } | TrayIconEvent::DoubleClick { .. }
                    );
                    if open {
                        let _ = events.send(DesktopEvent::Show(None));
                        wake();
                    }
                }));
            }
            MenuEvent::set_event_handler(Some(move |ev: MenuEvent| {
                if ev.id == show_id {
                    let _ = events.send(DesktopEvent::Show(None));
                } else if ev.id == quit_id {
                    let _ = events.send(DesktopEvent::Quit);
                }
                wake();
            }));
            Some(Tray { icon: tray })
        }

        pub fn set_tooltip(&self, text: &str) {
            let _ = self.icon.set_tooltip(Some(text));
        }
    }
}

/* ------------------------------ Diğer sistemler ------------------------------ */

#[cfg(not(windows))]
mod imp {
    use super::*;
    use std::path::Path;

    pub fn autostart_enabled(_profile_id: &str) -> bool {
        false
    }

    pub fn set_autostart(_profile_id: &str, _enabled: bool) -> std::io::Result<()> {
        Err(std::io::Error::other("Bu sistemde desteklenmiyor"))
    }

    pub fn refresh_autostart(_profile_id: &str) {}

    pub fn notify(
        _icon_dir: &Path,
        _title: &str,
        _body: &str,
        _peer_id: Option<String>,
        _events: Sender<DesktopEvent>,
        _wake: impl Fn() + Send + Sync + Clone + 'static,
    ) {
    }

    pub struct Tray;

    impl Tray {
        pub fn new(
            _tooltip: &str,
            _events: Sender<DesktopEvent>,
            _wake: impl Fn() + Send + Sync + Clone + 'static,
        ) -> Option<Tray> {
            None
        }

        pub fn set_tooltip(&self, _text: &str) {}
    }
}

pub use imp::{Tray, autostart_enabled, notify, refresh_autostart, set_autostart};

/// Bildirimde gösterilecek kısa metin (çok uzunsa kısaltılır).
pub fn preview(text: &str) -> String {
    let one_line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = one_line.chars().take(160).collect();
    if one_line.chars().count() > 160 {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autostart_command_quotes_path() {
        let cmd = autostart_command(std::path::Path::new(r"C:\Program Files\LanChat.exe"), "abc");
        assert_eq!(
            cmd,
            r#""C:\Program Files\LanChat.exe" --hesap-id abc --arka-plan"#
        );
        assert_eq!(autostart_value_name("abc"), "LanChat-abc");
    }

    #[test]
    fn preview_is_single_line_and_short() {
        assert_eq!(preview("merhaba\n  dünya"), "merhaba dünya");
        let long = "a".repeat(500);
        let p = preview(&long);
        assert_eq!(p.chars().count(), 161);
        assert!(p.ends_with('…'));
    }
}
