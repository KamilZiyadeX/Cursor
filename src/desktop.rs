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

/// Bildirim sesi
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub enum SoundKind {
    /// LanChat'in kendi sesi ("dıdıt dıt")
    LanChat,
    /// Windows'un anlık ileti sesi
    Windows,
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

    /// Başlat menüsüne, bildirimler için gereken uygulama kimliğini (AUMID)
    /// taşıyan bir LanChat kısayolu koyar. Windows, kurulum yapılmamış
    /// programların bildirimlerini ancak böyle bir kısayol varsa gösterir.
    fn ensure_start_menu_shortcut() -> windows::core::Result<()> {
        use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
        use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PropVariantClear};
        use windows::Win32::System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
            IPersistFile,
        };
        use windows::Win32::System::Variant::VT_LPWSTR;
        use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
        use windows::Win32::UI::Shell::{IShellLinkW, SHStrDupW, ShellLink};
        use windows::core::{HSTRING, Interface};

        let (Ok(exe), Ok(appdata)) = (std::env::current_exe(), std::env::var("APPDATA")) else {
            return Ok(());
        };
        let dir = Path::new(&appdata).join(r"Microsoft\Windows\Start Menu\Programs");
        let _ = std::fs::create_dir_all(&dir);
        let lnk = dir.join("LanChat.lnk");
        // SAFETY: COM nesneleri bu iş parçacığında oluşturulup burada kullanılır;
        // PROPVARIANT'ın metni SHStrDupW ile ayrılır ve PropVariantClear ile serbest bırakılır.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            link.SetPath(&HSTRING::from(exe.as_os_str()))?;
            if let Some(parent) = exe.parent() {
                link.SetWorkingDirectory(&HSTRING::from(parent.as_os_str()))?;
            }
            link.SetDescription(&HSTRING::from(
                "LanChat: aynı ağdaki bilgisayarlar arasında mesaj ve dosya",
            ))?;
            let store: IPropertyStore = link.cast()?;
            let mut pv = PROPVARIANT::default();
            (*pv.Anonymous.Anonymous).vt = VT_LPWSTR;
            (*pv.Anonymous.Anonymous).Anonymous.pwszVal = SHStrDupW(&HSTRING::from(AUMID))?;
            let set = store
                .SetValue(&PKEY_AppUserModel_ID, &pv)
                .and_then(|_| store.Commit());
            let _ = PropVariantClear(&mut pv);
            set?;
            let file: IPersistFile = link.cast()?;
            file.Save(&HSTRING::from(lnk.as_os_str()), true)?;
        }
        Ok(())
    }

    /// Bildirimlerin çıkabilmesi için gerekenleri hazırlar (açılışta, arka planda).
    pub fn init_notifications(icon_dir: &Path) {
        let _ = app_id(icon_dir);
    }

    /// Bildirimlerde "LanChat" adı ve logosu görünsün diye uygulama kimliğini kaydeder.
    fn app_id(icon_dir: &Path) -> String {
        let mut guard = APP_ID.lock().unwrap();
        if let Some(id) = guard.as_ref() {
            return id.clone();
        }
        let _ = ensure_start_menu_shortcut();
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

    /// Bildirim sesini çalar (beklemeden, arka planda).
    pub fn play_sound(kind: SoundKind) {
        use std::sync::OnceLock;
        use windows_sys::Win32::Media::Audio::{
            PlaySoundW, SND_ALIAS, SND_ASYNC, SND_MEMORY, SND_NODEFAULT,
        };
        // SND_ASYNC ile çalınan bellekteki ses, çalma bitene kadar yerinde
        // durmalıdır: bu yüzden program boyunca yaşayan bir değişkende tutulur.
        static WAV: OnceLock<Vec<u8>> = OnceLock::new();
        match kind {
            SoundKind::LanChat => {
                let wav = WAV.get_or_init(crate::sound::notification_wav);
                // SAFETY: `wav` 'static ömürlüdür; SND_MEMORY ile işaretçi WAV verisini gösterir.
                unsafe {
                    PlaySoundW(
                        wav.as_ptr().cast(),
                        std::ptr::null_mut(),
                        SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
                    );
                }
            }
            SoundKind::Windows => {
                let alias = wide("Notification.IM");
                // SAFETY: SND_ALIAS ile ad, çağrı sırasında okunur; dizi o sürece geçerli.
                unsafe {
                    PlaySoundW(alias.as_ptr(), std::ptr::null_mut(), SND_ALIAS | SND_ASYNC);
                }
            }
        }
    }

    pub fn notify(
        icon_dir: &Path,
        title: &str,
        body: &str,
        sound: Option<SoundKind>,
        peer_id: Option<String>,
        events: Sender<DesktopEvent>,
        wake: impl Fn() + Send + Sync + Clone + 'static,
    ) {
        use tauri_winrt_notification::Toast;
        // Ses bildirimden bağımsız çalınır: bildirim gösterilemese bile duyulsun
        if let Some(kind) = sound {
            play_sound(kind);
        }
        let icon_dir = icon_dir.to_path_buf();
        let (title, body) = (title.to_string(), body.to_string());
        // WinRT çağrıları kısa da olsa bekletebilir: arayüzü durdurmasın
        std::thread::spawn(move || {
            let id = app_id(&icon_dir);
            let make = |app: &str| {
                let events = events.clone();
                let peer_id = peer_id.clone();
                let wake = wake.clone();
                Toast::new(app)
                    .title(&title)
                    .text1(&body)
                    .sound(None)
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
        normal: tray_icon::Icon,
        blank: tray_icon::Icon,
        /// Şu an boş simge mi gösteriliyor?
        blank_shown: bool,
        tooltip: String,
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
            let blank =
                tray_icon::Icon::from_rgba(vec![0; (size * size * 4) as usize], size, size).ok()?;
            let tray = TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_menu_on_left_click(false)
                .with_tooltip(tooltip)
                .with_icon(icon.clone())
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
            Some(Tray {
                icon: tray,
                normal: icon,
                blank,
                blank_shown: false,
                tooltip: tooltip.to_string(),
            })
        }

        pub fn set_tooltip(&mut self, text: &str) {
            if self.tooltip != text {
                self.tooltip = text.to_string();
                let _ = self.icon.set_tooltip(Some(text));
            }
        }

        /// Okunmamış mesaj varken simgeyi yanıp söndürür.
        pub fn set_blinking(&mut self, blinking: bool) {
            let blank = blinking && blink_phase();
            if blank != self.blank_shown {
                self.blank_shown = blank;
                let icon = if blank { &self.blank } else { &self.normal };
                let _ = self.icon.set_icon(Some(icon.clone()));
            }
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

    pub fn init_notifications(_icon_dir: &Path) {}

    pub fn play_sound(_kind: SoundKind) {}

    pub fn notify(
        _icon_dir: &Path,
        _title: &str,
        _body: &str,
        _sound: Option<SoundKind>,
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

        pub fn set_tooltip(&mut self, _text: &str) {}

        pub fn set_blinking(&mut self, _blinking: bool) {}
    }
}

pub use imp::{
    Tray, autostart_enabled, init_notifications, notify, play_sound, refresh_autostart,
    set_autostart,
};

/// Yanıp sönme aralığı
pub const BLINK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(600);

/// Yanıp sönmenin şu anki yarısı (true: simge gizli)
#[cfg_attr(not(windows), allow(dead_code))]
fn blink_phase() -> bool {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    (ms / BLINK_INTERVAL.as_millis()) % 2 == 1
}

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
