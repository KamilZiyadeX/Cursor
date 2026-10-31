//! Masaüstü arayüzü (egui).

use crate::net::{self, Net, NetEvent, NetOptions, format_bytes};
use crate::protocol;
use crate::storage::{Body, ChatMessage, Config, History, KnownPeer, MsgState};
use chrono::{Local, TimeZone};
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Frame, Id, Key, Layout, Margin, Modifiers,
    RichText, ScrollArea, Stroke, TextEdit,
};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

const ONLINE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_HISTORY: usize = 5000;
const INPUT_ID: &str = "composer-input";

struct Peer {
    id: String,
    name: String,
    addr: Option<SocketAddr>,
    last_seen: Option<Instant>,
}

impl Peer {
    fn online(&self) -> bool {
        self.last_seen.is_some_and(|t| t.elapsed() < ONLINE_TIMEOUT)
    }
}

struct Settings {
    name: String,
    download_dir: PathBuf,
    manual_ips: Vec<String>,
    new_ip: String,
    error: Option<String>,
}

pub struct LanChatApp {
    data_dir: PathBuf,
    cfg: Config,
    net: Option<Net>,
    rx: Option<Receiver<NetEvent>>,
    startup_error: Option<String>,

    peers: Vec<Peer>,
    messages: Vec<ChatMessage>,
    selected: Option<String>,
    input: String,
    unread: HashMap<String, usize>,

    dirty: bool,
    last_save: Instant,
    settings: Option<Settings>,
    toast: Option<(String, Instant)>,
    title: String,
    focused: bool,
}

impl LanChatApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_style(&cc.egui_ctx);

        let data_dir = crate::storage::data_dir();
        let cfg = Config::load_or_create(&data_dir);
        let history = History::load(&data_dir);

        let ctx = cc.egui_ctx.clone();
        let tcp_port = std::env::var("LANCHAT_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(protocol::DEFAULT_TCP_PORT);
        let started = Net::start(
            NetOptions {
                my_id: cfg.id.clone(),
                name: cfg.name.clone(),
                download_dir: cfg.download_dir.clone(),
                tcp_port,
                discovery_port: protocol::DISCOVERY_PORT,
                manual_ips: cfg.manual_ips.iter().filter_map(|s| parse_ip(s)).collect(),
            },
            move || ctx.request_repaint(),
        );
        let (net, rx, startup_error) = match started {
            Ok((n, r)) => (Some(n), Some(r), None),
            Err(e) => (None, None, Some(format!("Ağ başlatılamadı: {e}"))),
        };

        let peers: Vec<Peer> = history
            .peers
            .into_iter()
            .map(|p| Peer {
                id: p.id,
                name: p.name,
                addr: p.addr,
                last_seen: None,
            })
            .collect();
        let selected = history.messages.last().map(|m| m.peer_id.clone());

        Self {
            data_dir,
            cfg,
            net,
            rx,
            startup_error,
            peers,
            messages: history.messages,
            selected,
            input: String::new(),
            unread: HashMap::new(),
            dirty: false,
            last_save: Instant::now(),
            settings: None,
            toast: None,
            title: String::new(),
            focused: true,
        }
    }

    /* ------------------------------ Durum ------------------------------ */

    fn peer_mut(&mut self, id: &str) -> Option<&mut Peer> {
        self.peers.iter_mut().find(|p| p.id == id)
    }

    fn peer(&self, id: &str) -> Option<&Peer> {
        self.peers.iter().find(|p| p.id == id)
    }

    fn upsert_peer(&mut self, id: &str, name: &str, addr: SocketAddr) {
        let changed = match self.peer_mut(id) {
            Some(p) => {
                let changed = p.name != name || p.addr != Some(addr);
                p.name = name.to_string();
                p.addr = Some(addr);
                p.last_seen = Some(Instant::now());
                changed
            }
            None => {
                self.peers.push(Peer {
                    id: id.to_string(),
                    name: name.to_string(),
                    addr: Some(addr),
                    last_seen: Some(Instant::now()),
                });
                if self.selected.is_none() {
                    self.selected = Some(id.to_string());
                }
                true
            }
        };
        self.dirty |= changed;
    }

    fn message_mut(&mut self, id: &str) -> Option<&mut ChatMessage> {
        self.messages.iter_mut().rev().find(|m| m.id == id)
    }

    fn push_message(&mut self, msg: ChatMessage) {
        self.messages.push(msg);
        if self.messages.len() > MAX_HISTORY {
            let extra = self.messages.len() - MAX_HISTORY;
            self.messages.drain(..extra);
        }
        self.dirty = true;
    }

    fn on_incoming(&mut self, ctx: &egui::Context, peer_id: &str) {
        let viewing = self.focused && self.selected.as_deref() == Some(peer_id);
        if !viewing {
            *self.unread.entry(peer_id.to_string()).or_default() += 1;
        }
        if !self.focused {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                egui::UserAttentionType::Informational,
            ));
        }
    }

    fn handle_event(&mut self, ctx: &egui::Context, ev: NetEvent) {
        match ev {
            NetEvent::PeerSeen { id, name, addr } => self.upsert_peer(&id, &name, addr),
            NetEvent::IncomingText {
                from,
                addr,
                msg_id,
                text,
            } => {
                self.upsert_peer(&from.id, &from.name, addr);
                self.push_message(ChatMessage::new(
                    msg_id,
                    from.id.clone(),
                    false,
                    Body::Text(text),
                    MsgState::Done,
                ));
                self.on_incoming(ctx, &from.id);
            }
            NetEvent::IncomingTransfer {
                from,
                addr,
                msg_id,
                name,
                is_dir,
                total_size,
                file_count,
            } => {
                self.upsert_peer(&from.id, &from.name, addr);
                if let Some(m) = self.message_mut(&msg_id) {
                    // Yarım kalan aktarım kaldığı yerden devam ediyor
                    m.state = MsgState::Pending;
                    m.progress = 0;
                    m.speed.reset();
                    if let Body::File {
                        size,
                        file_count: c,
                        ..
                    } = &mut m.body
                    {
                        *size = total_size;
                        *c = file_count;
                    }
                    self.dirty = true;
                } else {
                    self.push_message(ChatMessage::new(
                        msg_id,
                        from.id.clone(),
                        false,
                        Body::File {
                            name,
                            size: total_size,
                            path: None,
                            is_dir,
                            file_count,
                        },
                        MsgState::Pending,
                    ));
                    self.on_incoming(ctx, &from.id);
                }
            }
            NetEvent::Scanned {
                msg_id,
                total_size,
                file_count,
            } => {
                if let Some(m) = self.message_mut(&msg_id)
                    && let Body::File {
                        size,
                        file_count: c,
                        ..
                    } = &mut m.body
                {
                    *size = total_size;
                    *c = file_count;
                    self.dirty = true;
                }
            }
            NetEvent::Progress { msg_id, done } => {
                if let Some(m) = self.message_mut(&msg_id) {
                    m.progress = done;
                    m.speed.update(done);
                }
            }
            NetEvent::Completed {
                msg_id,
                saved_path,
                note,
            } => {
                if let Some(m) = self.message_mut(&msg_id) {
                    m.state = MsgState::Done;
                    m.note = note;
                    if let (Some(p), Body::File { path, .. }) = (saved_path, &mut m.body) {
                        *path = Some(p);
                    }
                    self.dirty = true;
                }
            }
            NetEvent::Failed { msg_id, error } => {
                if let Some(m) = self.message_mut(&msg_id) {
                    m.state = MsgState::Failed(error);
                    self.dirty = true;
                }
            }
        }
    }

    fn save_history(&mut self) {
        let history = History {
            peers: self
                .peers
                .iter()
                .map(|p| KnownPeer {
                    id: p.id.clone(),
                    name: p.name.clone(),
                    addr: p.addr,
                })
                .collect(),
            messages: self.messages.clone(),
        };
        if let Err(e) = history.save(&self.data_dir) {
            self.show_toast(format!("Geçmiş kaydedilemedi: {e}"));
        }
        self.dirty = false;
        self.last_save = Instant::now();
    }

    fn show_toast(&mut self, text: impl Into<String>) {
        self.toast = Some((text.into(), Instant::now()));
    }

    /* ------------------------------ Gönderme ------------------------------ */

    fn target_addr(&mut self) -> Option<(String, SocketAddr)> {
        let Some(peer_id) = self.selected.clone() else {
            self.show_toast("Önce soldan bir kişi seçin");
            return None;
        };
        match self.peer(&peer_id).and_then(|p| p.addr) {
            Some(addr) => Some((peer_id, addr)),
            None => {
                self.show_toast("Bu kişinin adresi bilinmiyor");
                None
            }
        }
    }

    fn send_text(&mut self) {
        let text = self.input.trim_end().to_string();
        if text.trim().is_empty() {
            return;
        }
        let Some(net) = self.net.clone() else { return };
        let Some((peer_id, addr)) = self.target_addr() else {
            return;
        };
        let id = new_id();
        self.push_message(ChatMessage::new(
            id.clone(),
            peer_id,
            true,
            Body::Text(text.clone()),
            MsgState::Pending,
        ));
        net.send_text(addr, id, text);
        self.input.clear();
    }

    /// Dosyaları ve/veya klasörleri gönderir.
    fn send_paths(&mut self, paths: Vec<PathBuf>) {
        let Some(net) = self.net.clone() else { return };
        let Some((peer_id, addr)) = self.target_addr() else {
            return;
        };
        for path in paths {
            let Ok(meta) = std::fs::metadata(&path) else {
                self.show_toast(format!("Okunamadı: {}", path.display()));
                continue;
            };
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            let is_dir = meta.is_dir();
            let id = new_id();
            self.push_message(ChatMessage::new(
                id.clone(),
                peer_id.clone(),
                true,
                Body::File {
                    name,
                    // Klasörün boyutu tarama bitince belli olur
                    size: if is_dir { 0 } else { meta.len() },
                    path: Some(path.clone()),
                    is_dir,
                    file_count: if is_dir { 0 } else { 1 },
                },
                MsgState::Pending,
            ));
            net.send_path(addr, id, path);
        }
    }

    /// Başarısız mesajı yeniden gönderir; dosya/klasörde kaldığı yerden devam eder.
    fn retry(&mut self, msg_id: &str) {
        let Some(net) = self.net.clone() else { return };
        let Some(m) = self.messages.iter().find(|m| m.id == msg_id).cloned() else {
            return;
        };
        let Some(addr) = self.peer(&m.peer_id).and_then(|p| p.addr) else {
            self.show_toast("Bu kişinin adresi bilinmiyor");
            return;
        };
        if let Some(mm) = self.message_mut(msg_id) {
            mm.state = MsgState::Pending;
            mm.progress = 0;
            mm.note = None;
            mm.speed.reset();
        }
        match m.body {
            Body::Text(t) => net.send_text(addr, m.id, t),
            Body::File { path: Some(p), .. } => net.send_path(addr, m.id, p),
            Body::File { path: None, .. } => {}
        }
    }

    /* ------------------------------ Çizim ------------------------------ */

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("💬 LanChat").strong().size(18.0));
            ui.separator();
            ui.label(RichText::new(format!("Adın: {}", self.cfg.name)).weak());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("⚙ Ayarlar").clicked() {
                    self.settings = Some(Settings {
                        name: self.cfg.name.clone(),
                        download_dir: self.cfg.download_dir.clone(),
                        manual_ips: self.cfg.manual_ips.clone(),
                        new_ip: String::new(),
                        error: None,
                    });
                }
                if ui
                    .button("📂 Gelen dosyalar")
                    .on_hover_text(self.cfg.download_dir.display().to_string())
                    .clicked()
                {
                    let _ = std::fs::create_dir_all(&self.cfg.download_dir);
                    open_path(&self.cfg.download_dir);
                }
            });
        });
    }

    fn peer_list(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.label(RichText::new("Kişiler").strong());
        ui.add_space(4.0);

        let mut order: Vec<usize> = (0..self.peers.len()).collect();
        order.sort_by(|&a, &b| {
            let (pa, pb) = (&self.peers[a], &self.peers[b]);
            pb.online()
                .cmp(&pa.online())
                .then_with(|| pa.name.to_lowercase().cmp(&pb.name.to_lowercase()))
        });

        ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
            if self.peers.is_empty() {
                ui.label(
                    RichText::new("Henüz kimse yok.\n\nAynı ağdaki diğer bilgisayarda da LanChat'i açın; birkaç saniye içinde burada görünecek.")
                        .weak(),
                );
            }
            for i in order {
                let p = &self.peers[i];
                let online = p.online();
                let unread = self.unread.get(&p.id).copied().unwrap_or(0);
                let selected = self.selected.as_deref() == Some(p.id.as_str());
                let dot = if online { Color32::from_rgb(34, 197, 94) } else { Color32::GRAY };
                let mut text = egui::text::LayoutJob::default();
                text.append("● ", 0.0, egui::TextFormat::simple(FontId::proportional(14.0), dot));
                text.append(
                    &p.name,
                    0.0,
                    egui::TextFormat::simple(FontId::proportional(15.0), ui.visuals().text_color()),
                );
                if unread > 0 {
                    text.append(
                        &format!("  ({unread})"),
                        0.0,
                        egui::TextFormat::simple(FontId::proportional(14.0), ui.visuals().hyperlink_color),
                    );
                }
                let hover = match (online, p.addr) {
                    (true, Some(a)) => format!("Çevrimiçi — {}", a.ip()),
                    (false, Some(a)) => format!("Çevrimdışı — son adres {}", a.ip()),
                    _ => "Çevrimdışı".into(),
                };
                let resp = ui
                    .add_sized([ui.available_width(), 28.0], egui::Button::selectable(selected, text))
                    .on_hover_text(hover);
                if resp.clicked() {
                    let id = p.id.clone();
                    self.unread.remove(&id);
                    self.selected = Some(id);
                    ui.memory_mut(|m| m.request_focus(Id::unique(INPUT_ID)));
                }
            }
        });

        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            ui.add_space(6.0);
            if let Some(net) = &self.net {
                let ips = net::local_ipv4_addresses();
                let ip_text = if ips.is_empty() {
                    "IP bulunamadı".to_string()
                } else {
                    ips.iter()
                        .map(|i| i.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                ui.label(
                    RichText::new(format!("Bu bilgisayar: {ip_text}"))
                        .small()
                        .weak(),
                )
                .on_hover_text(format!("Mesaj portu: {}", net.tcp_port()));
                if let Some(err) = net.discovery_error() {
                    ui.label(
                        RichText::new(err)
                            .small()
                            .color(Color32::from_rgb(234, 88, 12)),
                    );
                }
            }
            ui.separator();
        });
    }

    fn composer(&mut self, ui: &mut egui::Ui) {
        let input_id = Id::unique(INPUT_ID);
        // Enter gönderir, Shift+Enter yeni satır ekler
        let enter_pressed = ui.memory(|m| m.has_focus(input_id))
            && ui.input_mut(|i| !i.modifiers.shift && i.consume_key(Modifiers::NONE, Key::Enter));

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let mut picked: Option<Vec<PathBuf>> = None;
            ui.menu_button(RichText::new("📎").size(18.0), |ui| {
                if ui.button("📄  Dosya gönder…").clicked() {
                    ui.close();
                    picked = rfd::FileDialog::new()
                        .set_title("Gönderilecek dosyaları seçin")
                        .pick_files();
                }
                if ui.button("🗂  Klasör gönder…").clicked() {
                    ui.close();
                    picked = rfd::FileDialog::new()
                        .set_title("Gönderilecek klasörleri seçin")
                        .pick_folders();
                }
            })
            .response
            .on_hover_text("Dosya ya da klasör gönder (pencereye sürükleyip de bırakabilirsiniz)");
            if let Some(paths) = picked {
                self.send_paths(paths);
            }
            let send_w = 80.0;
            let edit = TextEdit::multiline(&mut self.input)
                .id(input_id)
                .desired_rows(2)
                .desired_width(ui.available_width() - send_w - 8.0)
                .hint_text("Mesaj yazın…  (Enter: gönder, Shift+Enter: yeni satır)");
            ui.add(edit);
            let send = ui.add_sized(
                [send_w, 36.0],
                egui::Button::new(RichText::new("Gönder ➤").strong()),
            );
            if send.clicked() || enter_pressed {
                self.send_text();
                ui.memory_mut(|m| m.request_focus(input_id));
            }
        });
        ui.add_space(6.0);
    }

    fn chat(&mut self, ui: &mut egui::Ui) {
        let Some(peer_id) = self.selected.clone() else {
            ui.centered_and_justified(|ui| {
                ui.label(
                    RichText::new("Mesajlaşmak için soldan bir kişi seçin")
                        .weak()
                        .size(16.0),
                );
            });
            return;
        };

        let (name, online) = self
            .peer(&peer_id)
            .map(|p| (p.name.clone(), p.online()))
            .unwrap_or_else(|| ("?".into(), false));

        egui::Panel::top("chat-header").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(&name).strong().size(16.0));
                let (txt, col) = if online {
                    ("çevrimiçi", Color32::from_rgb(34, 197, 94))
                } else {
                    ("çevrimdışı", Color32::GRAY)
                };
                ui.label(RichText::new(txt).color(col).small());
            });
            ui.add_space(4.0);
        });

        egui::Panel::bottom("composer").show(ui, |ui| self.composer(ui));

        let mut actions: Vec<Action> = Vec::new();
        egui::CentralPanel::default().show(ui, |ui| {
            ScrollArea::vertical()
                .stick_to_bottom(true)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add_space(8.0);
                    let mut last_day = String::new();
                    let msgs: Vec<&ChatMessage> = self.messages.iter().filter(|m| m.peer_id == peer_id).collect();
                    if msgs.is_empty() {
                        ui.vertical_centered(|ui| {
                            ui.add_space(40.0);
                            ui.label(RichText::new("Henüz mesaj yok. Bir mesaj yazın ya da dosyaları pencereye sürükleyip bırakın.").weak());
                        });
                    }
                    for m in msgs {
                        let day = format_day(m.ts);
                        if day != last_day {
                            ui.vertical_centered(|ui| {
                                ui.add_space(6.0);
                                ui.label(RichText::new(&day).small().weak());
                                ui.add_space(2.0);
                            });
                            last_day = day;
                        }
                        message_bubble(ui, m, &mut actions);
                        ui.add_space(4.0);
                    }
                    ui.add_space(4.0);
                });
        });

        for a in actions {
            match a {
                Action::Retry(id) => self.retry(&id),
                Action::Cancel(id) => {
                    if let Some(net) = &self.net {
                        net.cancel(&id);
                    }
                }
                Action::Copy(t) => {
                    ui.ctx().copy_text(t);
                    self.show_toast("Kopyalandı");
                }
            }
        }
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        let Some(s) = &mut self.settings else { return };
        let mut close = false;
        let mut apply = false;

        egui::Window::new("Ayarlar")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(420.0)
            .show(ctx, |ui| {
                ui.label(RichText::new("Adın").strong());
                ui.label(RichText::new("Diğer bilgisayarlarda bu ad görünür (ör. “Kamil – Ofis”).").small().weak());
                ui.add(TextEdit::singleline(&mut s.name).char_limit(40).desired_width(f32::INFINITY));
                ui.add_space(10.0);

                ui.label(RichText::new("Gelen dosyaların kaydedileceği klasör").strong());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(s.download_dir.display().to_string()).monospace().small());
                    if ui.button("Değiştir…").clicked()
                        && let Some(d) = rfd::FileDialog::new().set_directory(&s.download_dir).pick_folder() {
                            s.download_dir = d;
                        }
                });
                ui.add_space(10.0);

                ui.label(RichText::new("Elle eklenen bilgisayarlar").strong());
                ui.label(
                    RichText::new("Karşı bilgisayar otomatik görünmüyorsa IP adresini buraya ekleyin (karşı tarafta sol altta yazar).")
                        .small()
                        .weak(),
                );
                let mut remove = None;
                for (i, ip) in s.manual_ips.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(ip).monospace());
                        if ui.small_button("✖").on_hover_text("Kaldır").clicked() {
                            remove = Some(i);
                        }
                    });
                }
                if let Some(i) = remove {
                    s.manual_ips.remove(i);
                }
                ui.horizontal(|ui| {
                    let r = ui.add(TextEdit::singleline(&mut s.new_ip).hint_text("ör. 192.168.1.42").desired_width(200.0));
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    if ui.button("➕ Ekle").clicked() || enter {
                        let v = s.new_ip.trim().to_string();
                        if parse_ip(&v).is_some() {
                            if !s.manual_ips.contains(&v) {
                                s.manual_ips.push(v);
                            }
                            s.new_ip.clear();
                            s.error = None;
                        } else {
                            s.error = Some("Geçersiz IP adresi".into());
                        }
                    }
                });
                if let Some(e) = &s.error {
                    ui.colored_label(Color32::from_rgb(220, 38, 38), e);
                }

                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button(RichText::new("Kaydet").strong()).clicked() {
                        apply = true;
                    }
                    if ui.button("İptal").clicked() {
                        close = true;
                    }
                });
            });

        if apply {
            let s = self.settings.take().unwrap();
            let name: String = s.name.trim().chars().take(40).collect();
            if !name.is_empty() {
                self.cfg.name = name;
            }
            self.cfg.download_dir = s.download_dir;
            self.cfg.manual_ips = s.manual_ips;
            if let Some(net) = &self.net {
                net.set_name(&self.cfg.name);
                net.set_download_dir(self.cfg.download_dir.clone());
                for ip in self.cfg.manual_ips.iter().filter_map(|s| parse_ip(s)) {
                    net.add_target(ip);
                }
            }
            match self.cfg.save(&self.data_dir) {
                Ok(()) => self.show_toast("Ayarlar kaydedildi"),
                Err(e) => self.show_toast(format!("Ayarlar kaydedilemedi: {e}")),
            }
        } else if close {
            self.settings = None;
        }
    }

    fn overlays(&mut self, ctx: &egui::Context) {
        let rect = ctx.content_rect();
        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                Id::unique("drop"),
            ));
            painter.rect_filled(rect, 0.0, Color32::from_black_alpha(140));
            let who = self
                .selected
                .as_ref()
                .and_then(|id| self.peer(id))
                .map(|p| format!("“{}” kişisine göndermek için bırakın", p.name))
                .unwrap_or_else(|| "Önce bir kişi seçin".into());
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                who,
                FontId::proportional(22.0),
                Color32::WHITE,
            );
        }

        if let Some((text, at)) = &self.toast {
            if at.elapsed() > Duration::from_millis(2500) {
                self.toast = None;
            } else {
                egui::Area::new(Id::unique("toast"))
                    .anchor(Align2::CENTER_BOTTOM, [0.0, -90.0])
                    .order(egui::Order::Tooltip)
                    .interactable(false)
                    .show(ctx, |ui| {
                        Frame::new()
                            .fill(Color32::from_rgb(17, 24, 39))
                            .corner_radius(CornerRadius::same(8))
                            .inner_margin(Margin::symmetric(14, 8))
                            .show(ui, |ui| {
                                ui.label(RichText::new(text.as_str()).color(Color32::WHITE))
                            });
                    });
                ctx.request_repaint_after(Duration::from_millis(300));
            }
        }
    }
}

enum Action {
    Retry(String),
    Cancel(String),
    Copy(String),
}

fn message_bubble(ui: &mut egui::Ui, m: &ChatMessage, actions: &mut Vec<Action>) {
    let dark = ui.visuals().dark_mode;
    let fill = match (m.outgoing, dark) {
        (true, false) => Color32::from_rgb(219, 234, 254),
        (true, true) => Color32::from_rgb(30, 58, 138),
        (false, false) => Color32::WHITE,
        (false, true) => Color32::from_rgb(39, 39, 42),
    };
    let max_w = (ui.available_width() * 0.72).max(220.0);
    // Giden mesajlar sağa yaslanır. egui'de sağa hizalı kaplar genişlediği için
    // balonun genişliği bir önceki karede ölçülüp soldan boşluk bırakılır.
    let width_id = Id::unique(("bubble-width", m.id.as_str()));
    let prev_w: Option<f32> = ui.data(|d| d.get_temp(width_id));

    ui.horizontal_top(|ui| {
        let gap = ui.spacing().item_spacing.x;
        if m.outgoing {
            let w = prev_w.unwrap_or(max_w);
            ui.add_space((ui.available_width() - w - gap - 6.0).max(0.0));
        } else {
            ui.add_space(6.0);
        }
        let resp = Frame::new()
            .fill(fill)
            .stroke(Stroke::new(
                1.0,
                ui.visuals().widgets.noninteractive.bg_stroke.color,
            ))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.set_max_width(max_w);
                ui.vertical(|ui| {
                    match &m.body {
                        Body::Text(t) => {
                            ui.add(egui::Label::new(RichText::new(t).size(15.0)).wrap());
                        }
                        Body::File { .. } => file_body(ui, m, actions),
                    }
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format_time(m.ts)).small().weak());
                        if m.outgoing {
                            match &m.state {
                                MsgState::Pending => {
                                    if matches!(m.body, Body::Text(_)) {
                                        ui.label(RichText::new("gönderiliyor…").small().weak());
                                    }
                                }
                                MsgState::Done => {
                                    ui.label(
                                        RichText::new("✔")
                                            .small()
                                            .color(Color32::from_rgb(34, 197, 94)),
                                    )
                                    .on_hover_text("Teslim edildi");
                                }
                                MsgState::Failed(e) => {
                                    ui.label(
                                        RichText::new("⚠ gönderilemedi")
                                            .small()
                                            .color(Color32::from_rgb(220, 38, 38)),
                                    )
                                    .on_hover_text(e);
                                    let label = if matches!(m.body, Body::Text(_)) {
                                        "Tekrar dene"
                                    } else {
                                        "▶ Kaldığı yerden devam et"
                                    };
                                    if ui.small_button(label).clicked() {
                                        actions.push(Action::Retry(m.id.clone()));
                                    }
                                }
                            }
                        } else if let MsgState::Failed(e) = &m.state {
                            ui.label(
                                RichText::new("⚠ alınamadı")
                                    .small()
                                    .color(Color32::from_rgb(220, 38, 38)),
                            )
                            .on_hover_text(e);
                        }
                        if let Some(note) = &m.note {
                            ui.label(
                                RichText::new(format!("ⓘ {note}"))
                                    .small()
                                    .color(Color32::from_rgb(234, 88, 12)),
                            );
                        }
                    });
                });
            })
            .response;

        let w = resp.rect.width();
        if prev_w.is_none_or(|p| (p - w).abs() > 0.5) {
            ui.data_mut(|d| d.insert_temp(width_id, w));
            ui.ctx().request_repaint();
        }

        if let Body::Text(t) = &m.body {
            resp.context_menu(|ui| {
                if ui.button("📋 Kopyala").clicked() {
                    actions.push(Action::Copy(t.clone()));
                    ui.close();
                }
            });
        }
    });
}

fn file_body(ui: &mut egui::Ui, m: &ChatMessage, actions: &mut Vec<Action>) {
    let Body::File {
        name,
        size,
        path,
        is_dir,
        file_count,
    } = &m.body
    else {
        return;
    };
    let (size, is_dir) = (*size, *is_dir);
    let scanning =
        is_dir && m.outgoing && size == 0 && *file_count == 0 && m.state == MsgState::Pending;

    ui.horizontal(|ui| {
        ui.label(RichText::new(if is_dir { "🗂" } else { file_icon(name) }).size(26.0));
        ui.vertical(|ui| {
            ui.add(egui::Label::new(RichText::new(name).strong()).wrap());
            let info = if scanning {
                "klasör taranıyor…".to_string()
            } else if is_dir {
                format!(
                    "{} dosya · {}",
                    format_count(*file_count),
                    format_bytes(size)
                )
            } else {
                format_bytes(size)
            };
            ui.label(RichText::new(info).small().weak());
        });
    });

    match &m.state {
        MsgState::Pending => {
            let frac = if size == 0 {
                if scanning { 0.0 } else { 1.0 }
            } else {
                (m.progress as f64 / size as f64).min(1.0) as f32
            };
            ui.add(
                egui::ProgressBar::new(frac)
                    .desired_width(300.0)
                    .text(format!(
                        "{}  /  {}",
                        format_bytes(m.progress.min(size)),
                        format_bytes(size)
                    )),
            );
            ui.horizontal(|ui| {
                let bps = m.speed.bytes_per_sec;
                if bps > 0.0 {
                    let left = size.saturating_sub(m.progress) as f64 / bps;
                    ui.label(
                        RichText::new(format!(
                            "{}/sn · kalan {}",
                            format_bytes(bps as u64),
                            format_eta(left)
                        ))
                        .small()
                        .weak(),
                    );
                }
                if ui.small_button("✖ İptal").clicked() {
                    actions.push(Action::Cancel(m.id.clone()));
                }
            });
        }
        MsgState::Done => {
            if let Some(p) = path {
                ui.horizontal(|ui| {
                    if !m.outgoing && ui.button("Aç").clicked() {
                        open_path(p);
                    }
                    if ui.button("Klasörde göster").clicked() {
                        reveal_path(p);
                    }
                });
            }
        }
        MsgState::Failed(_) => {
            if !m.outgoing {
                ui.label(
                    RichText::new("Gönderen “Kaldığı yerden devam et” derse aktarım sürer.")
                        .small()
                        .weak(),
                );
            }
        }
    }
}

/* ------------------------------ eframe ------------------------------ */

impl eframe::App for LanChatApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(rx) = self.rx.take() {
            while let Ok(ev) = rx.try_recv() {
                self.handle_event(ctx, ev);
            }
            self.rx = Some(rx);
        }

        self.focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        if self.focused
            && let Some(sel) = &self.selected
        {
            self.unread.remove(sel);
        }
        let total: usize = self.unread.values().sum();
        let title = if total > 0 {
            format!("({total}) LanChat")
        } else {
            "LanChat".to_string()
        };
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }

        if self.dirty && self.last_save.elapsed() > Duration::from_secs(2) {
            self.save_history();
        }
        // Çevrimiçi/çevrimdışı durumunu güncel tutmak için
        ctx.request_repaint_after(Duration::from_secs(1));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if !dropped.is_empty() {
            self.send_paths(dropped);
        }

        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(6.0);
            self.top_bar(ui);
            if let Some(err) = &self.startup_error {
                ui.colored_label(Color32::from_rgb(220, 38, 38), err);
            }
            ui.add_space(6.0);
        });

        egui::Panel::left("peers")
            .resizable(true)
            .default_size(230.0)
            .size_range(160.0..=400.0)
            .show(ui, |ui| self.peer_list(ui));

        egui::CentralPanel::default()
            .frame(Frame::new().fill(ui.visuals().extreme_bg_color))
            .show(ui, |ui| self.chat(ui));

        self.settings_window(&ctx);
        self.overlays(&ctx);
    }

    fn on_exit(&mut self) {
        self.save_history();
    }
}

/* ------------------------------ Yardımcılar ------------------------------ */

fn setup_style(ctx: &egui::Context) {
    ctx.global_style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        for (ts, size) in [
            (egui::TextStyle::Body, 15.0),
            (egui::TextStyle::Button, 15.0),
            (egui::TextStyle::Small, 12.0),
            (egui::TextStyle::Monospace, 14.0),
        ] {
            if let Some(f) = style.text_styles.get_mut(&ts) {
                f.size = size;
            }
        }
    });
}

fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

fn parse_ip(s: &str) -> Option<IpAddr> {
    s.trim().parse().ok()
}

fn format_count(n: u64) -> String {
    // 1234567 -> "1.234.567"
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push('.');
        }
        out.push(c);
    }
    out
}

fn format_eta(secs: f64) -> String {
    if !secs.is_finite() {
        return "?".into();
    }
    let s = secs.round() as u64;
    if s < 60 {
        format!("~{s} sn")
    } else if s < 3600 {
        format!("~{} dk", s.div_ceil(60))
    } else if s < 48 * 3600 {
        format!("~{} sa {} dk", s / 3600, (s % 3600) / 60)
    } else {
        format!("~{} gün {} sa", s / 86400, (s % 86400) / 3600)
    }
}

fn format_time(ts: i64) -> String {
    Local
        .timestamp_opt(ts, 0)
        .single()
        .map(|d| d.format("%H:%M").to_string())
        .unwrap_or_default()
}

fn format_day(ts: i64) -> String {
    let Some(d) = Local.timestamp_opt(ts, 0).single() else {
        return String::new();
    };
    let today = Local::now().date_naive();
    let day = d.date_naive();
    if day == today {
        "Bugün".into()
    } else if Some(day) == today.pred_opt() {
        "Dün".into()
    } else {
        const MONTHS: [&str; 12] = [
            "Ocak", "Şubat", "Mart", "Nisan", "Mayıs", "Haziran", "Temmuz", "Ağustos", "Eylül",
            "Ekim", "Kasım", "Aralık",
        ];
        use chrono::Datelike;
        format!(
            "{} {} {}",
            day.day(),
            MONTHS[day.month0() as usize],
            day.year()
        )
    }
}

fn file_icon(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "heic" | "svg" => "🖼",
        "mp4" | "mov" | "avi" | "mkv" | "webm" => "🎞",
        "mp3" | "wav" | "flac" | "ogg" | "m4a" => "🎵",
        "zip" | "rar" | "7z" | "tar" | "gz" => "📦",
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "odt" => "📄",
        _ => "📁",
    }
}

fn open_path(p: &Path) {
    #[cfg(target_os = "windows")]
    let r = std::process::Command::new("explorer").arg(p).spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(p).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(p).spawn();
    let _ = r;
}

fn reveal_path(p: &Path) {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("explorer")
            .raw_arg(format!("/select,\"{}\"", p.display()))
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg("-R").arg(p).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let dir = p.parent().unwrap_or(p);
        let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_and_eta() {
        assert_eq!(format_count(7), "7");
        assert_eq!(format_count(1234), "1.234");
        assert_eq!(format_count(1234567), "1.234.567");
        assert_eq!(format_eta(42.0), "~42 sn");
        assert_eq!(format_eta(61.0), "~2 dk");
        assert_eq!(format_eta(3.0 * 3600.0 + 120.0), "~3 sa 2 dk");
        assert_eq!(format_eta(f64::INFINITY), "?");
    }
}
