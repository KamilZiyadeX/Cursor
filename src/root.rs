//! Açılış: hangi hesabın açılacağına karar verir, gerekirse hesap seçme ekranını gösterir.
//!
//! Komut satırı:
//!   lanchat --hesap "Kamil – İş"   bu adlı hesabı açar (yoksa oluşturur)
//!   lanchat --hesap-id <kimlik>    (uygulamanın kendi kullandığı) kimlikle açar
//!   --guncelleme                   güncelleme sonrası: eski sürümün kapanmasını bekler
//!   --arka-plan                    pencereyi göstermeden, tepside başlar (Windows açılışı)
//!
//! İstenen hesap başka bir pencerede zaten açıksa (tepside gizli olabilir) o
//! pencere öne getirilir ve bu yeni işlem kapanır.

use crate::app::{self, LanChatApp};
use crate::storage::{self, LockedProfile, Profile, ProfileError, ProfileInfo, UnlockError};
use eframe::egui::{self, Align, Color32, Key, Layout, RichText, TextEdit};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

enum Cli {
    None,
    Name(String),
    Id(String),
}

fn parse_cli() -> Cli {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--hesap-id" | "--profile-id" => {
                if let Some(v) = it.next() {
                    return Cli::Id(v.clone());
                }
            }
            "--hesap" | "--profile" => {
                if let Some(v) = it.next() {
                    return Cli::Name(v.clone());
                }
            }
            _ => {
                if let Some(v) = a.strip_prefix("--hesap=") {
                    return Cli::Name(v.to_string());
                }
            }
        }
    }
    Cli::None
}

struct Picker {
    profiles: Vec<ProfileInfo>,
    new_name: String,
    message: Option<String>,
    refreshed: Instant,
}

type UnlockResult = Result<Profile, (LockedProfile, UnlockError)>;

/// Şifreli bir hesabın giriş ekranı.
struct Unlock {
    name: String,
    dir: PathBuf,
    /// Şifre denenirken arka plan iş parçacığına geçer
    locked: Option<LockedProfile>,
    working: Option<Receiver<UnlockResult>>,
    password: String,
    error: Option<String>,
    confirm_reset: bool,
    focused: bool,
}

pub struct Root {
    root_dir: PathBuf,
    chat: Option<LanChatApp>,
    unlock: Option<Unlock>,
    picker: Picker,
    title_set: bool,
    logo: egui::TextureHandle,
}

impl Root {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        app::setup_style(&cc.egui_ctx);
        let root_dir = storage::root_dir();
        let mut root = Root {
            picker: Picker {
                profiles: storage::list_profiles(&root_dir),
                new_name: String::new(),
                message: None,
                refreshed: Instant::now(),
            },
            root_dir,
            chat: None,
            unlock: None,
            title_set: false,
            logo: app::large_logo_texture(&cc.egui_ctx),
        };
        root.startup(&cc.egui_ctx);
        root
    }

    fn startup(&mut self, ctx: &egui::Context) {
        let profiles = self.picker.profiles.clone();
        let find_name = |n: &str| {
            profiles
                .iter()
                .find(|p| p.name.to_lowercase() == n.trim().to_lowercase())
                .cloned()
        };
        match parse_cli() {
            Cli::Id(id) => match profiles.iter().find(|p| p.id == id) {
                Some(p) => {
                    let mut p = p.clone();
                    if std::env::args().any(|a| a == "--guncelleme") {
                        // Güncellemeden sonra: eski sürüm kapanıp hesabı bırakana kadar bekle
                        let start = Instant::now();
                        while p.in_use && start.elapsed() < Duration::from_secs(15) {
                            std::thread::sleep(Duration::from_millis(200));
                            if let Some(q) = storage::list_profiles(&self.root_dir)
                                .into_iter()
                                .find(|q| q.id == id)
                            {
                                p = q;
                            }
                        }
                        self.picker.profiles = storage::list_profiles(&self.root_dir);
                    }
                    self.try_open(ctx, &p)
                }
                None => self.picker.message = Some("İstenen hesap bulunamadı".into()),
            },
            Cli::Name(name) => match find_name(&name) {
                Some(p) => self.try_open(ctx, &p),
                None => self.create_and_open(ctx, &name),
            },
            Cli::None => {
                if profiles.is_empty() {
                    // İlk açılış: bilgisayar adıyla bir hesap oluştur
                    self.create_and_open(ctx, "");
                    return;
                }
                // Son kullanılan (yoksa ilk) hesap boştaysa doğrudan onu aç;
                // zaten açıksa hesap seçme ekranını göster.
                let last = storage::last_profile(&self.root_dir);
                let preferred = last
                    .and_then(|id| profiles.iter().find(|p| p.id == id).cloned())
                    .unwrap_or_else(|| profiles[0].clone());
                // Son kullanılan hesap zaten açıksa (tepside gizli olabilir) onu öne getirir
                self.try_open(ctx, &preferred);
            }
        }
    }

    fn try_open(&mut self, ctx: &egui::Context, p: &ProfileInfo) {
        match storage::open_profile(&p.dir) {
            Ok(locked) if locked.has_password() => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
                    "LanChat — {} (şifreli)",
                    locked.cfg.name
                )));
                self.unlock = Some(Unlock {
                    name: locked.cfg.name.clone(),
                    dir: locked.dir.clone(),
                    locked: Some(locked),
                    working: None,
                    password: String::new(),
                    error: None,
                    confirm_reset: false,
                    focused: false,
                });
            }
            Ok(locked) => match locked.unlock("") {
                Ok(profile) => self.start_chat(ctx, profile),
                Err((_, e)) => self.picker.message = Some(e.to_string()),
            },
            Err(ProfileError::InUse) => self.bring_to_front(ctx, p),
            Err(e) => {
                self.picker.message = Some(e.to_string());
                self.picker.profiles = storage::list_profiles(&self.root_dir);
            }
        }
    }

    /// Hesap başka bir pencerede açık: o pencereyi öne getir, bu pencereyi kapat.
    fn bring_to_front(&mut self, ctx: &egui::Context, p: &ProfileInfo) {
        storage::request_show(&p.dir);
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn start_chat(&mut self, ctx: &egui::Context, profile: Profile) {
        // Şifre girilerek açıldıysa kullanıcı zaten pencerenin başında: gizleme
        let hidden = self.unlock.is_none() && std::env::args().any(|a| a == "--arka-plan");
        self.unlock = None;
        self.chat = Some(LanChatApp::new(ctx, self.root_dir.clone(), profile, hidden));
    }

    fn unlock_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let Some(u) = &mut self.unlock else { return };

        // Arka planda denenen şifrenin sonucu
        if let Some(rx) = &u.working {
            match rx.try_recv() {
                Ok(Ok(profile)) => {
                    self.start_chat(&ctx, profile);
                    return;
                }
                Ok(Err((locked, e))) => {
                    u.locked = Some(locked);
                    u.working = None;
                    u.password.clear();
                    u.focused = false;
                    u.error = Some(match e {
                        UnlockError::WrongPassword => "Şifre yanlış. Tekrar deneyin.".into(),
                        other => other.to_string(),
                    });
                }
                Err(mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(50))
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    u.working = None;
                    u.error = Some("Hesap açılamadı".into());
                }
            }
        }

        let mut submit = false;
        let mut back = false;
        let mut reset = false;
        let busy = u.working.is_some();

        egui::CentralPanel::default().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(56.0);
                ui.add(egui::Image::new(&self.logo).fit_to_exact_size(egui::vec2(72.0, 72.0)));
                ui.add_space(10.0);
                ui.label(RichText::new(&u.name).size(24.0).strong());
                ui.add_space(4.0);

                if u.confirm_reset {
                    ui.add_space(10.0);
                    ui.label(RichText::new("Şifre kurtarılamaz").size(17.0).strong());
                    ui.add_space(6.0);
                    ui.allocate_ui(egui::vec2(380.0, 0.0), |ui| {
                        ui.add(egui::Label::new(
                            "Hesabı sıfırlarsanız bu hesabın sohbet geçmişi silinir ve hesabın \
                             şifreleme anahtarı yenilenir; eşleştiğiniz cihazlarla yeniden \
                             eşleşmeniz gerekir. Adınız, ayarlarınız ve indirilen dosyalar korunur.",
                        ).wrap());
                    });
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        ui.add_space((ui.available_width() - 250.0).max(0.0) / 2.0);
                        let danger = egui::Button::new(RichText::new("Hesabı sıfırla").strong().color(Color32::WHITE))
                            .fill(Color32::from_rgb(200, 30, 30));
                        if ui.add(danger).clicked() {
                            reset = true;
                        }
                        if ui.button("Vazgeç").clicked() {
                            u.confirm_reset = false;
                        }
                    });
                    return;
                }

                ui.label(RichText::new("🔒 Bu hesap şifreli").weak());
                ui.add_space(18.0);
                let r = ui.add_enabled(
                    !busy,
                    TextEdit::singleline(&mut u.password)
                        .password(true)
                        .hint_text("Şifre")
                        .desired_width(280.0),
                );
                if !u.focused {
                    r.request_focus();
                    u.focused = true;
                }
                if (r.has_focus() || r.lost_focus()) && ui.input(|i| i.key_pressed(Key::Enter)) {
                    submit = true;
                }
                ui.add_space(10.0);
                if busy {
                    ui.horizontal(|ui| {
                        ui.add_space((ui.available_width() - 110.0).max(0.0) / 2.0);
                        ui.spinner();
                        ui.label("Açılıyor…");
                    });
                } else if ui
                    .add_sized([280.0, 34.0], egui::Button::new(RichText::new("Aç").strong()))
                    .clicked()
                {
                    submit = true;
                }
                if let Some(e) = &u.error {
                    ui.add_space(8.0);
                    ui.colored_label(Color32::from_rgb(234, 88, 12), e);
                }
                ui.add_space(28.0);
                ui.horizontal(|ui| {
                    ui.add_space((ui.available_width() - 290.0).max(0.0) / 2.0);
                    if ui.add_enabled(!busy, egui::Button::new("← Başka hesap seç").frame(false)).clicked() {
                        back = true;
                    }
                    ui.add_space(24.0);
                    if ui.add_enabled(!busy, egui::Button::new("Şifremi unuttum").frame(false)).clicked() {
                        u.confirm_reset = true;
                    }
                });
            });
        });

        if submit && !busy && !u.password.is_empty() {
            if let Some(locked) = u.locked.take() {
                let (tx, rx) = mpsc::channel();
                let password = u.password.clone();
                let ctx2 = ctx.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(locked.unlock(&password));
                    ctx2.request_repaint();
                });
                u.working = Some(rx);
                u.error = None;
            }
        } else if back {
            // Kilidi bırak, hesap seçme ekranına dön
            self.unlock = None;
            self.title_set = false;
            self.picker.message = None;
            self.picker.profiles = storage::list_profiles(&self.root_dir);
        } else if reset && let Some(locked) = u.locked.take() {
            match locked.reset_forgotten() {
                Ok(profile) => self.start_chat(&ctx, profile),
                Err(e) => u.error = Some(format!("Hesap sıfırlanamadı: {e}")),
            }
        }
    }

    fn create_and_open(&mut self, ctx: &egui::Context, name: &str) {
        match storage::create_profile(&self.root_dir, name) {
            Ok(p) => self.try_open(ctx, &p),
            Err(e) => self.picker.message = Some(format!("Hesap oluşturulamadı: {e}")),
        }
    }

    fn picker_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if !self.title_set {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title("LanChat — hesap seç".into()));
            self.title_set = true;
        }
        // Başka pencerelerdeki hesapların açık/kapalı durumu değişebilir
        ctx.request_repaint_after(Duration::from_secs(2));
        if self.picker.refreshed.elapsed() >= Duration::from_secs(2) {
            self.picker.profiles = storage::list_profiles(&self.root_dir);
            self.picker.refreshed = Instant::now();
        }

        let mut open: Option<ProfileInfo> = None;
        let mut create: Option<String> = None;

        egui::CentralPanel::default().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.add(egui::Image::new(&self.logo).fit_to_exact_size(egui::vec2(72.0, 72.0)));
                ui.add_space(6.0);
                ui.label(RichText::new("LanChat").size(26.0).strong());
                ui.add_space(4.0);
                ui.label(RichText::new("Hangi hesapla açılsın?").size(16.0));
                ui.add_space(12.0);
                if let Some(m) = &self.picker.message {
                    ui.label(RichText::new(m).color(Color32::from_rgb(234, 88, 12)));
                    ui.add_space(8.0);
                }
            });

            let width = 380.0f32.min(ui.available_width());
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), 0.0),
                Layout::top_down(Align::Center),
                |ui| {
                    ui.set_max_width(width);
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.set_width(width);
                        if self.picker.profiles.is_empty() {
                            ui.label(RichText::new("Henüz hesap yok").weak());
                        }
                        for p in &self.picker.profiles {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(&p.name).size(15.0));
                                if p.has_password {
                                    ui.label(RichText::new("🔒").small().weak())
                                        .on_hover_text("Şifreli hesap");
                                }
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if p.in_use {
                                        ui.label(
                                            RichText::new("başka pencerede açık").small().weak(),
                                        );
                                    } else if ui.button(RichText::new("Aç").strong()).clicked() {
                                        open = Some(p.clone());
                                    }
                                });
                            });
                            ui.separator();
                        }
                        ui.add_space(4.0);
                        ui.label(RichText::new("Yeni hesap").strong());
                        ui.horizontal(|ui| {
                            let r = ui.add(
                                TextEdit::singleline(&mut self.picker.new_name)
                                    .hint_text("ör. Kamil – Kişisel")
                                    .char_limit(40)
                                    .desired_width(width - 110.0),
                            );
                            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                            if (ui.button("➕ Oluştur").clicked() || enter)
                                && !self.picker.new_name.trim().is_empty()
                            {
                                create = Some(self.picker.new_name.trim().to_string());
                            }
                        });
                    });
                },
            );
        });

        if let Some(p) = open {
            self.try_open(&ctx, &p);
        } else if let Some(name) = create {
            self.picker.new_name.clear();
            self.create_and_open(&ctx, &name);
        }
    }
}

impl eframe::App for Root {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if let Some(chat) = &mut self.chat {
            chat.logic(ctx, frame);
        } else if let Some(u) = &self.unlock
            && storage::take_show_request(&u.dir)
        {
            // Bu hesap için başka bir LanChat açılmak istendi: şifre ekranını öne getir
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        if self.chat.is_none() {
            ctx.request_repaint_after(Duration::from_secs(1));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if let Some(chat) = &mut self.chat {
            chat.ui(ui, frame);
        } else if self.unlock.is_some() {
            self.unlock_ui(ui);
        } else {
            self.picker_ui(ui);
        }
    }

    fn on_exit(&mut self) {
        if let Some(chat) = &mut self.chat {
            chat.on_exit();
        }
    }
}
