//! Masaüstü arayüzü (egui).

use crate::backup;
use crate::clip::{self, ClipContent};
use crate::desktop::{self, DesktopEvent};
use crate::net::{self, Net, NetEvent, NetOptions, format_bytes};
use crate::protocol::{self, ReplyRef};
use crate::secure::PublicKey;
use crate::storage::{
    self, Body, ChatMessage, Config, History, KnownPeer, MsgState, Outgoing, Profile, Signal,
    ThemeChoice, TrustedPeer,
};
use crate::update;
use chrono::{Local, TimeZone};
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Frame, Id, Key, Layout, Margin, Modifiers,
    RichText, ScrollArea, Stroke, TextEdit,
};
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

const ONLINE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_HISTORY: usize = 5000;
const INPUT_ID: &str = "composer-input";
const SEARCH_ID: &str = "chat-search";
/// "Herkese" sohbetinin kimliği (gerçek bir kişi değil)
const BROADCAST: &str = "__herkese__";

struct Peer {
    id: String,
    name: String,
    addr: Option<SocketAddr>,
    last_seen: Option<Instant>,
    /// Aynı bilgisayardaki (bu Windows kullanıcısının) başka bir hesap
    local: bool,
    /// Uzaktan uyandırma için ağ kartı adresleri
    macs: Vec<String>,
}

/// Bizim başlattığımız eşleştirme
struct OutPair {
    peer_id: String,
    name: String,
    code: Option<String>,
    /// Kullanıcı kodların aynı olduğunu onayladı mı?
    confirmed: bool,
    /// Karşı tarafın kararı ve anahtarı
    remote: Option<(bool, PublicKey)>,
    error: Option<String>,
}

/// Karşı tarafın başlattığı eşleştirme
struct InPair {
    from: protocol::From,
    addr: SocketAddr,
    code: String,
    key: PublicKey,
    reply: Sender<bool>,
    since: Instant,
}

impl Peer {
    fn online(&self) -> bool {
        self.last_seen.is_some_and(|t| t.elapsed() < ONLINE_TIMEOUT)
    }
}

struct NewAccount {
    name: String,
    error: Option<String>,
    focused: bool,
}

enum PasswordAction {
    Set(String),
    Change { current: String, new: String },
    Remove { current: String },
}

enum Thumb {
    Loading,
    Ready(egui::TextureHandle),
    Failed,
}

/// Önizleme gösterilecek resim dosyası mı?
fn is_image(name: &str) -> bool {
    let Some((_, ext)) = name.rsplit_once('.') else {
        return false;
    };
    let ext = ext.to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp"
    )
}

/// Önizleme için en fazla bu kadar büyük dosyalar çözülür.
const THUMB_MAX_FILE: u64 = 64 * 1024 * 1024;

/// Ağ üzerinden güncelleme durumu
enum UpdateState {
    Idle,
    Downloading { from: String, done: u64, total: u64 },
    Ready { path: PathBuf, version: String },
    Failed(String),
}

struct Settings {
    theme: ThemeChoice,
    zoom: f32,
    speed: net::SpeedLimit,
    close_to_tray: bool,
    notifications: bool,
    notification_sound: bool,
    windows_sound: bool,
    autostart: bool,
    has_password: bool,
    pw_current: String,
    pw_new: String,
    pw_repeat: String,
    pw_error: Option<String>,
    name: String,
    download_dir: PathBuf,
    manual_ips: Vec<String>,
    trusted: Vec<TrustedPeer>,
    new_ip: String,
    error: Option<String>,
}

pub struct LanChatApp {
    /// Tüm hesapların bulunduğu kök klasör
    root_dir: PathBuf,
    /// Bu hesabın klasörü
    data_dir: PathBuf,
    cfg: Config,
    /// Açık hesap; kilidi pencere kapanana kadar tutar
    profile: Profile,
    logo: egui::TextureHandle,
    new_account: Option<NewAccount>,
    /// Kim ne zamana kadar "yazıyor" görünecek
    typing: HashMap<String, Instant>,
    /// En son "yazıyor" bildirimini ne zaman gönderdik
    last_typing_sent: Option<Instant>,
    /// Yanıtlanan mesaj (hangi sohbette, alıntı)
    replying: Option<(String, ReplyRef)>,
    /// Düzenlenen mesaj (hangi sohbette, mesaj kimliği)
    editing: Option<(String, String)>,
    /// Pencere sallanıyor: (başlangıç, pencerenin asıl konumu)
    shake: Option<(Instant, egui::Pos2)>,
    /// Kişi başına son gelen/giden titreşim (art arda titreşimi sınırlamak için)
    last_nudge_in: HashMap<String, Instant>,
    last_nudge_out: HashMap<String, Instant>,
    /// Çevrimdışı kişilere iletilmeyi bekleyen bildirimler
    outbox: Vec<Outgoing>,
    /// Kişi başına son "sıradakileri gönder" denemesi
    last_flush: HashMap<String, Instant>,
    last_queue_check: Instant,
    /// Yapıştırılan resmin önizlemesi: (sohbet, resim, doku)
    paste_preview: Option<(String, egui::ColorImage, egui::TextureHandle)>,
    /// Dosya Gezgini'nden "Gönder → LanChat" ile gelen, kime gideceği sorulan dosyalar
    send_choice: Option<Vec<PathBuf>>,
    /// Pencereden dışarı sürüklenecek dosya (bu karenin sonunda başlatılır)
    drag_out: Option<PathBuf>,
    /// Dışarı sürükleme bitti: egui fare bırakılmasını görmediği için bildirilecek
    release_pointer: bool,
    /// Açık arama: (hangi sohbette, aranan metin)
    search: Option<(String, String)>,
    /// Arama kutusuna odaklanılsın mı (bir kerelik)
    search_focus: bool,
    /// Resim önizlemeleri (mesaj kimliğine göre)
    thumbs: HashMap<String, Thumb>,
    thumb_tx: Sender<(String, Option<egui::ColorImage>)>,
    thumb_rx: Receiver<(String, Option<egui::ColorImage>)>,
    /// Ağdaki cihazların LanChat sürümü ve platformu (kimliğe göre)
    peer_versions: HashMap<String, (String, String)>,
    update: UpdateState,
    /// "Daha sonra" denilen sürüm (bu oturumda yeniden sorulmaz)
    update_dismissed: Option<String>,
    /// Sistem tepsisi simgesi (yalnızca Windows)
    tray: Option<desktop::Tray>,
    desk_tx: Sender<DesktopEvent>,
    desk_rx: Receiver<DesktopEvent>,
    /// Pencere tepsiye gizlendi mi?
    hidden: bool,
    /// Gerçekten kapatılıyor (tepsiye gizlenmeyecek)
    quitting: bool,
    /// Açılışta pencereyi gizle (Windows açılınca arka planda başlatma)
    start_hidden: bool,
    last_show_check: Instant,
    out_pair: Option<OutPair>,
    in_pair: Option<InPair>,
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
    pub fn new(
        ctx: &egui::Context,
        root_dir: PathBuf,
        profile: Profile,
        start_hidden: bool,
    ) -> Self {
        let data_dir = profile.dir.clone();
        let cfg = profile.cfg.clone();
        let history = profile.load_history();
        let _ = storage::set_last_profile(&root_dir, &cfg.id);

        apply_appearance(ctx, &cfg);
        let local_registry = Some(storage::online_dir(&root_dir));
        let logo = small_logo_texture(ctx);
        let (thumb_tx, thumb_rx) = std::sync::mpsc::channel();
        let (desk_tx, desk_rx) = std::sync::mpsc::channel();
        let tray = {
            let ctx = ctx.clone();
            desktop::Tray::new(
                &format!("LanChat — {}", cfg.name),
                desk_tx.clone(),
                move || ctx.request_repaint(),
            )
        };
        desktop::refresh_autostart(&cfg.id);
        {
            // Bildirimler için Başlat menüsü kısayolu ve uygulama kimliği
            let dir = root_dir.clone();
            std::thread::spawn(move || desktop::init_notifications(&dir));
        }
        // Tepsi yoksa (ör. oluşturulamadıysa) pencereyi gizlemek onu kaybettirir
        let start_hidden = start_hidden && tray.is_some();
        let ctx = ctx.clone();
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
                local_registry,
                identity: profile.identity.clone(),
                trusted: cfg.trusted_map(),
                speed_limit: cfg.speed_limit,
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
                local: false,
                macs: p.macs,
            })
            .collect();
        let selected = history.messages.last().map(|m| m.peer_id.clone());

        Self {
            root_dir,
            data_dir,
            cfg,
            profile,
            logo,
            new_account: None,
            typing: HashMap::new(),
            last_typing_sent: None,
            replying: None,
            editing: None,
            shake: None,
            last_nudge_in: HashMap::new(),
            last_nudge_out: HashMap::new(),
            outbox: history.outbox,
            last_flush: HashMap::new(),
            last_queue_check: Instant::now(),
            paste_preview: None,
            send_choice: None,
            drag_out: None,
            release_pointer: false,
            search: None,
            search_focus: false,
            thumbs: HashMap::new(),
            thumb_tx,
            thumb_rx,
            peer_versions: HashMap::new(),
            update: UpdateState::Idle,
            update_dismissed: None,
            tray,
            desk_tx,
            desk_rx,
            hidden: false,
            quitting: false,
            start_hidden,
            last_show_check: Instant::now(),
            out_pair: None,
            in_pair: None,
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

    /// `local`: aynı bilgisayardaki başka bir hesap mı (None: bilinmiyor, değiştirme)
    fn upsert_peer(&mut self, id: &str, name: &str, addr: SocketAddr, local: Option<bool>) {
        let was_online = self.peer(id).is_some_and(|p| p.online());
        self.upsert_peer_inner(id, name, addr, local);
        if !was_online {
            // Çevrimiçi oldu: sırada bekleyenleri gönder
            self.flush_queue(id);
        }
    }

    fn upsert_peer_inner(&mut self, id: &str, name: &str, addr: SocketAddr, local: Option<bool>) {
        let changed = match self.peer_mut(id) {
            Some(p) => {
                let changed = p.name != name || p.addr != Some(addr);
                p.name = name.to_string();
                p.addr = Some(addr);
                p.last_seen = Some(Instant::now());
                if let Some(l) = local {
                    p.local = l;
                }
                changed
            }
            None => {
                self.peers.push(Peer {
                    id: id.to_string(),
                    name: name.to_string(),
                    addr: Some(addr),
                    last_seen: Some(Instant::now()),
                    local: local.unwrap_or(false),
                    macs: Vec::new(),
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

    /// Yeni mesaj ya da dosya geldiğinde: okunmamış sayısı, görev çubuğu ve bildirim.
    fn on_incoming(&mut self, ctx: &egui::Context, peer_id: &str, preview: &str) {
        let viewing = self.focused && self.selected.as_deref() == Some(peer_id);
        if !viewing {
            *self.unread.entry(peer_id.to_string()).or_default() += 1;
        }
        if !self.focused {
            if !self.hidden {
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                    egui::UserAttentionType::Informational,
                ));
            }
            if self.cfg.notifications {
                let name = self
                    .peer(peer_id)
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| "LanChat".into());
                self.notify(ctx, &name, preview, Some(peer_id.to_string()));
            }
        }
    }

    fn notify(&self, ctx: &egui::Context, title: &str, body: &str, peer_id: Option<String>) {
        let ctx = ctx.clone();
        desktop::notify(
            &self.root_dir,
            title,
            &desktop::preview(body),
            (self.cfg.notification_sound && peer_id.is_some())
                .then(|| sound_kind(self.cfg.windows_sound)),
            peer_id,
            self.desk_tx.clone(),
            move || ctx.request_repaint(),
        );
    }

    /// Pencereyi gösterir ve öne getirir.
    fn show_window(&mut self, ctx: &egui::Context) {
        self.hidden = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.request_repaint();
    }

    fn hide_window(&mut self, ctx: &egui::Context) {
        self.hidden = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    }

    /// Tepsi, bildirim, "öne gel" isteği ve pencere kapatma
    fn desktop_logic(&mut self, ctx: &egui::Context) {
        if std::mem::take(&mut self.start_hidden) {
            self.hide_window(ctx);
        }
        while let Ok(ev) = self.desk_rx.try_recv() {
            match ev {
                DesktopEvent::Show(peer) => {
                    if let Some(id) = peer {
                        self.unread.remove(&id);
                        self.selected = Some(id);
                    }
                    self.show_window(ctx);
                }
                DesktopEvent::Quit => {
                    self.quitting = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
        if self.last_show_check.elapsed() >= Duration::from_millis(700) {
            self.last_show_check = Instant::now();
            if storage::take_show_request(&self.data_dir) {
                self.show_window(ctx);
            }
            let paths = storage::take_send_requests(&self.data_dir);
            if !paths.is_empty() {
                self.send_choice.get_or_insert_with(Vec::new).extend(paths);
                self.show_window(ctx);
            }
        }
        let close = ctx.input(|i| i.viewport().close_requested());
        if close && !self.quitting && self.tray.is_some() && self.cfg.close_to_tray {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.hide_window(ctx);
            self.save_history();
            if !self.cfg.tray_hint_shown {
                self.cfg.tray_hint_shown = true;
                let _ = self.cfg.save(&self.data_dir);
                self.notify(
                    ctx,
                    "LanChat çalışmaya devam ediyor",
                    "Mesaj ve dosyalar gelmeye devam eder. Açmak için saatin yanındaki \
                     LanChat simgesine tıklayın; tamamen kapatmak için simgeye sağ tıklayıp \
                     Çıkış'ı seçin.",
                    None,
                );
            }
        }
    }

    fn handle_event(&mut self, ctx: &egui::Context, ev: NetEvent) {
        match ev {
            NetEvent::PeerSeen {
                id,
                name,
                addr,
                local,
            } => self.upsert_peer(&id, &name, addr, Some(local)),
            NetEvent::PairCode { peer_id, code } => {
                if let Some(op) = &mut self.out_pair
                    && op.peer_id == peer_id
                {
                    op.code = Some(code);
                }
            }
            NetEvent::PairResult {
                peer_id,
                accepted,
                public_key,
            } => {
                if let Some(op) = &mut self.out_pair
                    && op.peer_id == peer_id
                {
                    op.remote = Some((accepted, public_key));
                    if !accepted {
                        op.error = Some("Karşı taraf eşleştirmeyi reddetti".into());
                    }
                }
                self.finish_out_pair();
            }
            NetEvent::PairFailed { peer_id, error } => {
                if let Some(op) = &mut self.out_pair
                    && op.peer_id == peer_id
                {
                    op.error = Some(error);
                }
            }
            NetEvent::IncomingPair {
                from,
                addr,
                code,
                public_key,
                reply,
            } => {
                if self.in_pair.is_some() {
                    // Aynı anda tek istek
                    let _ = reply.send(false);
                } else {
                    self.upsert_peer(&from.id, &from.name, addr, None);
                    self.in_pair = Some(InPair {
                        from,
                        addr,
                        code,
                        key: public_key,
                        reply,
                        since: Instant::now(),
                    });
                    if !self.focused {
                        ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                            egui::UserAttentionType::Critical,
                        ));
                    }
                }
            }
            NetEvent::PeerVersion {
                id,
                version,
                platform,
                macs,
            } => {
                if let Some(p) = self.peer_mut(&id)
                    && !macs.is_empty()
                    && p.macs != macs
                {
                    p.macs = macs;
                    self.dirty = true;
                }
                self.peer_versions.insert(id, (version, platform));
            }
            NetEvent::UpdateProgress { done, total } => {
                if let UpdateState::Downloading {
                    done: d, total: t, ..
                } = &mut self.update
                {
                    *d = done;
                    *t = total;
                }
            }
            NetEvent::UpdateReady { path, version } => {
                self.update = UpdateState::Ready { path, version };
            }
            NetEvent::UpdateFailed { error } => {
                self.update = UpdateState::Failed(error);
            }
            NetEvent::Notice { text } => self.show_toast(text),
            NetEvent::Deleted { peer_id, msg_id } => {
                // Yalnızca o kişinin bize gönderdiği mesaj silinebilir
                if let Some(m) = self
                    .messages
                    .iter_mut()
                    .find(|m| m.id == msg_id && m.peer_id == peer_id && !m.outgoing)
                {
                    clear_message(m);
                    self.dirty = true;
                }
            }
            NetEvent::Edited {
                peer_id,
                msg_id,
                text,
            } => {
                // Yalnızca o kişinin bize gönderdiği metin mesajı düzeltilebilir
                if let Some(m) = self
                    .messages
                    .iter_mut()
                    .find(|m| m.id == msg_id && m.peer_id == peer_id && !m.outgoing && !m.deleted)
                    && matches!(m.body, Body::Text(_))
                {
                    m.body = Body::Text(text);
                    m.edited = true;
                    self.dirty = true;
                }
            }
            NetEvent::Reacted {
                peer_id,
                msg_id,
                emoji,
            } => {
                if let Some(m) = self
                    .messages
                    .iter_mut()
                    .find(|m| m.id == msg_id && m.peer_id == peer_id && !m.deleted)
                {
                    match emoji {
                        Some(e) => m.reactions.insert(peer_id, e),
                        None => m.reactions.remove(&peer_id),
                    };
                    self.dirty = true;
                }
            }
            NetEvent::Nudged { peer_id } => {
                // Art arda gelen titreşimler yok sayılır
                if self
                    .last_nudge_in
                    .get(&peer_id)
                    .is_some_and(|t| t.elapsed() < NUDGE_COOLDOWN - Duration::from_secs(2))
                {
                    return;
                }
                self.last_nudge_in.insert(peer_id.clone(), Instant::now());
                self.push_message(ChatMessage {
                    read_sent: true,
                    ..ChatMessage::new(
                        new_id(),
                        peer_id.clone(),
                        false,
                        Body::Nudge,
                        MsgState::Done,
                    )
                });
                self.on_incoming(ctx, &peer_id, "📳 Titreşim gönderdi");
                // MSN'deki gibi: pencere öne gelir, sohbet açılır ve pencere sallanır
                if self.hidden || !self.focused {
                    self.unread.remove(&peer_id);
                    self.selected = Some(peer_id);
                }
                self.show_window(ctx);
                self.start_shake(ctx);
            }
            NetEvent::Typing { peer_id } => {
                self.typing
                    .insert(peer_id, Instant::now() + Duration::from_secs(5));
            }
            NetEvent::Read { peer_id, msg_ids } => {
                for m in self.messages.iter_mut() {
                    if m.outgoing && m.peer_id == peer_id && !m.read && msg_ids.contains(&m.id) {
                        m.read = true;
                        self.dirty = true;
                    }
                }
            }
            NetEvent::IncomingText {
                from,
                addr,
                msg_id,
                text,
                reply_to,
            } => {
                self.upsert_peer(&from.id, &from.name, addr, None);
                self.typing.remove(&from.id);
                // Aynı mesaj ikinci kez gelirse (ör. onay ulaşmadan bağlantı koptuysa) yok say
                if self
                    .messages
                    .iter()
                    .any(|m| m.id == msg_id && m.peer_id == from.id)
                {
                    return;
                }
                let preview = text.clone();
                self.push_message(ChatMessage {
                    reply_to,
                    ..ChatMessage::new(
                        msg_id,
                        from.id.clone(),
                        false,
                        Body::Text(text),
                        MsgState::Done,
                    )
                });
                self.on_incoming(ctx, &from.id, &preview);
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
                self.upsert_peer(&from.id, &from.name, addr, None);
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
                    let preview = if is_dir {
                        format!(
                            "📁 {name} klasörünü gönderiyor ({})",
                            format_bytes(total_size)
                        )
                    } else {
                        format!(
                            "📎 {name} dosyasını gönderiyor ({})",
                            format_bytes(total_size)
                        )
                    };
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
                    self.on_incoming(ctx, &from.id, &preview);
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
            NetEvent::Failed {
                msg_id,
                error,
                unreachable,
            } => {
                if let Some(m) = self.message_mut(&msg_id) {
                    // Karşı bilgisayara bağlanılamadıysa (kapalıysa) sıraya döner;
                    // çevrimiçi olunca kendiliğinden yeniden gönderilir.
                    m.state = if unreachable && m.outgoing {
                        MsgState::Queued
                    } else {
                        MsgState::Failed(error)
                    };
                    m.speed.reset();
                    self.dirty = true;
                }
            }
        }
    }

    fn current_history(&self) -> History {
        History {
            peers: self
                .peers
                .iter()
                .map(|p| KnownPeer {
                    id: p.id.clone(),
                    name: p.name.clone(),
                    addr: p.addr,
                    macs: p.macs.clone(),
                })
                .collect(),
            messages: self.messages.clone(),
            outbox: self.outbox.clone(),
        }
    }

    fn save_history(&mut self) {
        let history = self.current_history();
        // Şifreli hesapta geçmiş şifreli yazılır
        if let Err(e) = self.profile.save_history(&history) {
            self.show_toast(format!("Geçmiş kaydedilemedi: {e}"));
        }
        self.dirty = false;
        self.last_save = Instant::now();
    }

    /// Bu kişiyle şifreli ve kimliği doğrulanmış konuşabilir miyiz?
    fn is_paired(&self, peer_id: &str) -> bool {
        self.cfg.is_trusted(peer_id) || self.peer(peer_id).is_some_and(|p| p.local)
    }

    fn save_trusted(&mut self) {
        if let Some(net) = &self.net {
            net.set_trusted(self.cfg.trusted_map());
        }
        if let Err(e) = self.cfg.save(&self.data_dir) {
            self.show_toast(format!("Ayarlar kaydedilemedi: {e}"));
        }
    }

    fn start_pairing(&mut self, peer_id: &str) {
        let Some(net) = self.net.clone() else { return };
        let Some(p) = self.peer(peer_id) else { return };
        let Some(addr) = p.addr else {
            self.show_toast("Bu kişinin adresi bilinmiyor");
            return;
        };
        self.out_pair = Some(OutPair {
            peer_id: peer_id.to_string(),
            name: p.name.clone(),
            code: None,
            confirmed: false,
            remote: None,
            error: None,
        });
        net.send_pair_request(addr, peer_id.to_string());
    }

    /// İki taraf da onayladıysa eşleşmeyi kaydeder.
    fn finish_out_pair(&mut self) {
        let Some(op) = &self.out_pair else { return };
        if let (true, Some((true, key))) = (op.confirmed, op.remote) {
            let (id, name) = (op.peer_id.clone(), op.name.clone());
            self.cfg.add_trusted(&id, &name, &key);
            self.save_trusted();
            self.out_pair = None;
            self.show_toast(format!(
                "🔒 “{name}” ile eşleşildi; artık konuşmalarınız şifreli"
            ));
        }
    }

    fn start_reply(&mut self, msg_id: &str, peer_name: &str) {
        let Some(m) = self.messages.iter().find(|m| m.id == msg_id) else {
            return;
        };
        let preview = match &m.body {
            Body::Text(t) => t.replace('\n', " "),
            Body::File { name, .. } => format!("📄 {name}"),
            Body::Nudge => return,
        };
        let reply = ReplyRef {
            id: m.id.clone(),
            author: if m.outgoing {
                self.cfg.name.clone()
            } else {
                peer_name.to_string()
            },
            preview: preview.chars().take(120).collect(),
        };
        self.replying = Some((m.peer_id.clone(), reply));
    }

    /// Gönderdiğimiz mesajı iki taraftan da siler.
    fn delete_for_all(&mut self, msg_id: &str) {
        let copies = self.copies_of(msg_id);
        let mut ids: Vec<&str> = copies.iter().map(|(_, id)| id.as_str()).collect();
        ids.push(msg_id);
        for m in self
            .messages
            .iter_mut()
            .filter(|m| ids.contains(&m.id.as_str()) && m.outgoing)
        {
            clear_message(m);
        }
        self.dirty = true;
        let mut offline = 0;
        for (peer, id) in copies {
            if !self.send_signal(&peer, Signal::Delete { msg_id: id }) {
                offline += 1;
            }
        }
        if offline > 0 {
            self.show_toast(
                "Mesaj sizden silindi; karşı taraf çevrimiçi olunca ondan da silinecek",
            );
        } else {
            self.show_toast("Mesaj iki taraftan da silindi");
        }
    }

    /// Bir mesajın karşı taraflara giden kopyaları: "Herkese" özetinde kişilere
    /// giden mesajlar, normal mesajda kendisi. (kişi kimliği, mesaj kimliği)
    fn copies_of(&self, msg_id: &str) -> Vec<(String, String)> {
        let Some(m) = self.messages.iter().find(|m| m.id == msg_id) else {
            return Vec::new();
        };
        if m.peer_id == BROADCAST {
            m.children
                .iter()
                .filter_map(|id| self.messages.iter().find(|x| &x.id == id))
                .map(|x| (x.peer_id.clone(), x.id.clone()))
                .collect()
        } else {
            vec![(m.peer_id.clone(), m.id.clone())]
        }
    }

    /// Kişiye kısa bir bildirim (düzeltme, tepki…) gönderir. Kişi çevrimdışıysa
    /// bildirim sıraya konur (titreşim hariç) ve `false` döner.
    fn send_signal(&mut self, peer_id: &str, signal: Signal) -> bool {
        let addr = self
            .peer(peer_id)
            .filter(|p| p.online())
            .and_then(|p| p.addr);
        let (Some(net), Some(addr), true) = (&self.net, addr, self.is_paired(peer_id)) else {
            History::queue_signal(&mut self.outbox, peer_id, signal);
            self.dirty = true;
            return false;
        };
        let peer = peer_id.to_string();
        match signal {
            Signal::Delete { msg_id } => net.send_delete(addr, peer, msg_id),
            Signal::Edit { msg_id, text } => net.send_edit(addr, peer, msg_id, text),
            Signal::React { msg_id, emoji } => net.send_react(addr, peer, msg_id, emoji),
            Signal::Nudge => net.send_nudge(addr, peer),
        }
        true
    }

    /// Kişi çevrimiçiyse sırada bekleyen mesajlarını ve bildirimlerini gönderir.
    fn flush_queue(&mut self, peer_id: &str) {
        if !self.peer(peer_id).is_some_and(|p| p.online()) || !self.is_paired(peer_id) {
            return;
        }
        self.last_flush.insert(peer_id.to_string(), Instant::now());
        let queued: Vec<String> = self
            .messages
            .iter()
            .filter(|m| m.peer_id == peer_id && m.outgoing && m.state == MsgState::Queued)
            .map(|m| m.id.clone())
            .collect();
        for id in queued {
            self.dispatch(&id);
        }
        let (mine, rest): (Vec<Outgoing>, Vec<Outgoing>) = std::mem::take(&mut self.outbox)
            .into_iter()
            .partition(|o| o.peer_id == peer_id);
        self.outbox = rest;
        if !mine.is_empty() {
            self.dirty = true;
        }
        for o in mine {
            self.send_signal(&o.peer_id, o.signal);
        }
    }

    /// Bağlanılamayan ama listede çevrimiçi görünen kişiler için arada bir yeniden dener.
    fn retry_queues(&mut self) {
        let due: Vec<String> = self
            .peers
            .iter()
            .filter(|p| p.online())
            .filter(|p| {
                self.last_flush
                    .get(&p.id)
                    .is_none_or(|t| t.elapsed() > QUEUE_RETRY)
            })
            .filter(|p| {
                self.outbox.iter().any(|o| o.peer_id == p.id)
                    || self
                        .messages
                        .iter()
                        .any(|m| m.peer_id == p.id && m.state == MsgState::Queued)
            })
            .map(|p| p.id.clone())
            .collect();
        for id in due {
            self.flush_queue(&id);
        }
    }

    /// Giden mesajı gönderir; kişi çevrimdışıysa sıraya koyar.
    fn dispatch(&mut self, msg_id: &str) {
        let Some(m) = self.messages.iter().find(|m| m.id == msg_id).cloned() else {
            return;
        };
        let addr = self
            .peer(&m.peer_id)
            .filter(|p| p.online())
            .and_then(|p| p.addr);
        let (Some(net), Some(addr)) = (self.net.clone(), addr) else {
            if let Some(mm) = self.message_mut(msg_id) {
                mm.state = MsgState::Queued;
                mm.speed.reset();
            }
            self.dirty = true;
            return;
        };
        if let Some(mm) = self.message_mut(msg_id) {
            mm.state = MsgState::Pending;
            mm.progress = 0;
            mm.note = None;
            mm.speed.reset();
        }
        match m.body {
            Body::Text(t) => net.send_reply(addr, m.peer_id, m.id, t, m.reply_to),
            Body::File { path: Some(p), .. } => net.send_path(addr, m.peer_id, m.id, p),
            Body::File { path: None, .. } | Body::Nudge => {}
        }
    }

    /// Mesaja tepki verir; aynı tepki seçiliyse kaldırır.
    fn react(&mut self, msg_id: &str, emoji: &str) {
        let me = self.cfg.id.clone();
        let Some(m) = self.messages.iter().find(|m| m.id == msg_id) else {
            return;
        };
        let new =
            (m.reactions.get(&me).map(String::as_str) != Some(emoji)).then(|| emoji.to_string());
        let copies = self.copies_of(msg_id);
        let mut ids: Vec<&str> = copies.iter().map(|(_, id)| id.as_str()).collect();
        ids.push(msg_id);
        for m in self
            .messages
            .iter_mut()
            .filter(|m| ids.contains(&m.id.as_str()))
        {
            match &new {
                Some(e) => m.reactions.insert(me.clone(), e.clone()),
                None => m.reactions.remove(&me),
            };
        }
        self.dirty = true;
        let mut offline = 0;
        for (peer, id) in copies {
            let signal = Signal::React {
                msg_id: id,
                emoji: new.clone(),
            };
            if !self.send_signal(&peer, signal) {
                offline += 1;
            }
        }
        if offline > 0 {
            self.show_toast("Karşı taraf çevrimdışı; tepkiniz çevrimiçi olunca iletilecek");
        }
    }

    /// Panodakini (dosyalar, resim ya da metin) seçili sohbete gönderir.
    fn send_clipboard(&mut self, ctx: &egui::Context) {
        if self.selected.is_none() {
            self.show_toast("Önce soldan bir kişi seçin");
            return;
        }
        match clip::read() {
            Ok(ClipContent::Files(paths)) => self.send_paths(paths),
            Ok(ClipContent::Image(img)) => self.preview_paste(ctx, img),
            Ok(ClipContent::Text(text)) => {
                // Yazılmakta olan mesaj kaybolmasın
                let draft = std::mem::replace(&mut self.input, text);
                let editing = self.editing.take();
                self.send_text();
                self.input = draft;
                self.editing = editing;
            }
            Ok(ClipContent::Empty) => self.show_toast("Pano boş"),
            Err(e) => self.show_toast(e),
        }
    }

    /// Yapıştırılan resmi göndermeden önce gösterir.
    fn preview_paste(&mut self, ctx: &egui::Context, img: egui::ColorImage) {
        let Some(chat) = self.selected.clone() else {
            return;
        };
        if chat != BROADCAST && !self.is_paired(&chat) {
            self.show_toast("Önce bu kişiyle eşleşmelisiniz");
            return;
        }
        let tex = ctx.load_texture(
            "yapistirilan-resim",
            img.clone(),
            egui::TextureOptions::LINEAR,
        );
        self.paste_preview = Some((chat, img, tex));
    }

    /// "Gönder → LanChat" ile gelen dosyalar için kişi seçme penceresi.
    fn send_choice_window(&mut self, ctx: &egui::Context) {
        let Some(paths) = &self.send_choice else {
            return;
        };
        let mut targets: Vec<(String, String, bool)> = self.broadcast_targets();
        targets.sort_by(|a, b| {
            b.2.cmp(&a.2)
                .then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase()))
        });
        let mut chosen: Option<String> = None;
        let mut cancel = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
        egui::Window::new("Kime gönderilsin?")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                let names: Vec<String> = paths
                    .iter()
                    .map(|p| {
                        p.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| p.display().to_string())
                    })
                    .collect();
                let summary = match names.len() {
                    1 => format!("“{}”", names[0]),
                    n => format!("{n} öğe ({}…)", names[0]),
                };
                ui.label(RichText::new(summary).strong());
                ui.add_space(6.0);
                if targets.is_empty() {
                    ui.label(RichText::new("Henüz eşleştiğiniz bir cihaz yok.").weak());
                }
                ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    if targets.len() > 1
                        && ui
                            .add_sized([280.0, 30.0], egui::Button::new("📢 Herkese"))
                            .clicked()
                    {
                        chosen = Some(BROADCAST.to_string());
                    }
                    for (id, name, online) in &targets {
                        let dot = if *online {
                            Color32::from_rgb(34, 197, 94)
                        } else {
                            Color32::GRAY
                        };
                        let mut label = egui::text::LayoutJob::default();
                        label.append(
                            "● ",
                            0.0,
                            egui::TextFormat::simple(FontId::proportional(14.0), dot),
                        );
                        label.append(
                            name,
                            0.0,
                            egui::TextFormat::simple(
                                FontId::proportional(15.0),
                                ui.visuals().text_color(),
                            ),
                        );
                        let b = ui
                            .add_sized([280.0, 30.0], egui::Button::new(label))
                            .on_hover_text(if *online {
                                "Çevrimiçi: hemen gönderilir"
                            } else {
                                "Çevrimdışı: çevrimiçi olunca gönderilir"
                            });
                        if b.clicked() {
                            chosen = Some(id.clone());
                        }
                    }
                });
                ui.add_space(6.0);
                if ui.button("Vazgeç").clicked() {
                    cancel = true;
                }
            });
        if let Some(id) = chosen {
            let paths = self.send_choice.take().unwrap_or_default();
            self.unread.remove(&id);
            self.selected = Some(id);
            self.send_paths(paths);
        } else if cancel {
            self.send_choice = None;
        }
    }

    fn paste_preview_window(&mut self, ctx: &egui::Context) {
        let Some((chat, _, tex)) = &self.paste_preview else {
            return;
        };
        let to = if chat == BROADCAST {
            "📢 Herkese".to_string()
        } else {
            self.peer(chat).map(|p| p.name.clone()).unwrap_or_default()
        };
        let mut send = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
        let mut cancel = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
        egui::Window::new("Resmi gönder")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(format!("Panodaki resim “{to}” sohbetine gönderilsin mi?"));
                ui.add_space(6.0);
                ui.add(
                    egui::Image::new(tex)
                        .max_size(egui::vec2(460.0, 320.0))
                        .corner_radius(6),
                );
                let [w, h] = tex.size();
                ui.label(RichText::new(format!("{w} × {h} piksel")).small().weak());
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button(RichText::new("Gönder").strong()).clicked() {
                        send = true;
                    }
                    if ui.button("Vazgeç").clicked() {
                        cancel = true;
                    }
                    ui.label(RichText::new("Enter: gönder · Esc: vazgeç").small().weak());
                });
            });
        if cancel {
            self.paste_preview = None;
        } else if send {
            let Some((chat, img, _)) = self.paste_preview.take() else {
                return;
            };
            let dir = self.cfg.download_dir.join("Gönderilen ekran görüntüleri");
            match clip::save_png(&img, &dir) {
                Ok(path) => {
                    let prev = self.selected.replace(chat);
                    self.send_paths(vec![path]);
                    if self.selected != prev {
                        self.selected = prev;
                    }
                }
                Err(e) => self.show_toast(format!("Resim kaydedilemedi: {e}")),
            }
        }
    }

    /// Kapalı bilgisayarı Wake-on-LAN ile uyandırmayı dener.
    fn wake_peer(&mut self, peer_id: &str) {
        let Some(p) = self.peer(peer_id) else { return };
        match net::wake_on_lan(&p.macs, p.addr.map(|a| a.ip())) {
            Ok(_) => self.show_toast(format!(
                "“{}” için uyandırma sinyali gönderildi. Bilgisayarın açılması bir iki dakika sürebilir.",
                p.name
            )),
            Err(e) => self.show_toast(format!("Uyandırma sinyali gönderilemedi: {e}")),
        }
    }

    /// Seçili kişiye titreşim gönderir.
    fn send_nudge(&mut self, ctx: &egui::Context) {
        let Some(peer_id) = self.selected.clone() else {
            return;
        };
        if let Some(t) = self.last_nudge_out.get(&peer_id) {
            let left = NUDGE_COOLDOWN.saturating_sub(t.elapsed());
            if !left.is_zero() {
                self.show_toast(format!(
                    "Biraz bekleyin: {} sn sonra yeniden titreşim gönderebilirsiniz",
                    left.as_secs() + 1
                ));
                return;
            }
        }
        if !self.send_signal(&peer_id, Signal::Nudge) {
            self.show_toast("Kişi çevrimdışı; titreşim gönderilemedi");
            return;
        }
        self.last_nudge_out.insert(peer_id.clone(), Instant::now());
        self.push_message(ChatMessage::new(
            new_id(),
            peer_id,
            true,
            Body::Nudge,
            MsgState::Done,
        ));
        self.start_shake(ctx);
    }

    /// Pencereyi kısa bir süre sallar.
    fn start_shake(&mut self, ctx: &egui::Context) {
        if self.shake.is_some() {
            return;
        }
        if let Some(rect) = ctx.input(|i| i.viewport().outer_rect) {
            self.shake = Some((Instant::now(), rect.min));
            ctx.request_repaint();
        }
    }

    fn shake_logic(&mut self, ctx: &egui::Context) {
        let Some((start, origin)) = self.shake else {
            return;
        };
        let t = start.elapsed().as_secs_f32();
        let total = SHAKE_TIME.as_secs_f32();
        if t >= total {
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(origin));
            self.shake = None;
            return;
        }
        let amp = 14.0 * (1.0 - t / total);
        let dx = (t * std::f32::consts::TAU * 16.0).sin() * amp;
        let dy = (t * std::f32::consts::TAU * 11.0).cos() * amp * 0.5;
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(
            origin + egui::vec2(dx, dy),
        ));
        ctx.request_repaint();
    }

    fn start_edit(&mut self, msg_id: &str) {
        let Some(m) = self
            .messages
            .iter()
            .find(|m| m.id == msg_id && m.outgoing && !m.deleted)
        else {
            return;
        };
        let Body::Text(text) = &m.body else { return };
        let Some(chat) = self.selected.clone() else {
            return;
        };
        self.input = text.clone();
        self.replying = None;
        self.editing = Some((chat, msg_id.to_string()));
    }

    /// Düzenlenen mesajı kaydeder ve karşı taraflarda da düzeltir.
    fn apply_edit(&mut self, msg_id: &str, text: String) {
        let copies = self.copies_of(msg_id);
        let mut ids: Vec<&str> = copies.iter().map(|(_, id)| id.as_str()).collect();
        ids.push(msg_id);
        let mut changed = false;
        for m in self
            .messages
            .iter_mut()
            .filter(|m| ids.contains(&m.id.as_str()) && m.outgoing && !m.deleted)
        {
            if matches!(&m.body, Body::Text(t) if *t != text) {
                m.body = Body::Text(text.clone());
                m.edited = true;
                changed = true;
            }
        }
        if !changed {
            return;
        }
        self.dirty = true;
        let mut offline = 0;
        for (peer, id) in copies {
            let signal = Signal::Edit {
                msg_id: id,
                text: text.clone(),
            };
            if !self.send_signal(&peer, signal) {
                offline += 1;
            }
        }
        if offline > 0 {
            self.show_toast(
                "Mesaj sizde düzeltildi; karşı taraf çevrimiçi olunca onda da düzelecek",
            );
        }
    }

    /// Görünen resim mesajlarının önizlemesini arka planda hazırlar.
    fn request_thumbs(&mut self, ctx: &egui::Context, ids: Vec<String>) {
        for id in ids {
            if self.thumbs.contains_key(&id) {
                continue;
            }
            let Some(Body::File {
                path: Some(path), ..
            }) = self.messages.iter().find(|m| m.id == id).map(|m| &m.body)
            else {
                continue;
            };
            if std::fs::metadata(path).map_or(true, |m| m.len() > THUMB_MAX_FILE) {
                self.thumbs.insert(id, Thumb::Failed);
                continue;
            }
            // Bellek sınırı: çok fazla önizleme biriktiyse eskileri at
            if self.thumbs.len() > 300 {
                self.thumbs.clear();
            }
            self.thumbs.insert(id.clone(), Thumb::Loading);
            let (path, tx, ctx) = (path.clone(), self.thumb_tx.clone(), ctx.clone());
            std::thread::spawn(move || {
                let img = image::open(&path).ok().map(|img| {
                    // Yüksek çözünürlüklü ekranlar için gösterim boyutunun iki katı
                    let t = img.thumbnail(560, 440).to_rgba8();
                    egui::ColorImage::from_rgba_unmultiplied(
                        [t.width() as usize, t.height() as usize],
                        t.as_raw(),
                    )
                });
                let _ = tx.send((id, img));
                ctx.request_repaint();
            });
        }
    }

    fn receive_thumbs(&mut self, ctx: &egui::Context) {
        while let Ok((id, img)) = self.thumb_rx.try_recv() {
            let t = match img {
                Some(img) => Thumb::Ready(ctx.load_texture(
                    format!("thumb-{id}"),
                    img,
                    egui::TextureOptions::LINEAR,
                )),
                None => Thumb::Failed,
            };
            self.thumbs.insert(id, t);
        }
    }

    /// Yazarken karşı tarafa en fazla 3 saniyede bir "yazıyor" bildirir.
    fn notify_typing(&mut self) {
        if self
            .last_typing_sent
            .is_some_and(|t| t.elapsed() < Duration::from_secs(3))
        {
            return;
        }
        let Some(peer_id) = self.selected.clone() else {
            return;
        };
        if !self.is_paired(&peer_id) {
            return;
        }
        if let (Some(net), Some(addr)) = (&self.net, self.peer(&peer_id).and_then(|p| p.addr)) {
            net.send_typing(addr, peer_id);
            self.last_typing_sent = Some(Instant::now());
        }
    }

    /// Pencere öndeyken açık sohbetteki yeni gelen mesajlar için "okundu" gönderir.
    fn send_read_receipts(&mut self) {
        if !self.focused {
            return;
        }
        let Some(peer_id) = self.selected.clone() else {
            return;
        };
        let ids: Vec<String> = self
            .messages
            .iter()
            .filter(|m| !m.outgoing && !m.read_sent && m.peer_id == peer_id)
            .map(|m| m.id.clone())
            .collect();
        if ids.is_empty() {
            return;
        }
        for m in self.messages.iter_mut() {
            if !m.outgoing && m.peer_id == peer_id {
                m.read_sent = true;
            }
        }
        self.dirty = true;
        if !self.is_paired(&peer_id) {
            return;
        }
        if let (Some(net), Some(addr)) = (&self.net, self.peer(&peer_id).and_then(|p| p.addr)) {
            net.send_read(addr, peer_id, ids);
        }
    }

    fn show_toast(&mut self, text: impl Into<String>) {
        self.toast = Some((text.into(), Instant::now()));
    }

    /* ------------------------------ Gönderme ------------------------------ */

    /// Seçili sohbetin kişisi (eşleşmiş olmalı; çevrimdışıysa mesajlar sıraya girer).
    fn target_peer(&mut self) -> Option<String> {
        let Some(peer_id) = self.selected.clone() else {
            self.show_toast("Önce soldan bir kişi seçin");
            return None;
        };
        if !self.is_paired(&peer_id) {
            self.show_toast("Önce bu kişiyle eşleşmelisiniz: üstteki “🔑 Eşleştir”e basın");
            return None;
        }
        Some(peer_id)
    }

    /// "Herkese" gönderiminde alıcılar: eşleşmiş herkes (kimlik, ad, çevrimiçi mi).
    /// Çevrimdışı olanlara giden kopyalar sıraya girer.
    fn broadcast_targets(&self) -> Vec<(String, String, bool)> {
        self.peers
            .iter()
            .filter(|p| p.local || self.cfg.is_trusted(&p.id))
            .map(|p| (p.id.clone(), p.name.clone(), p.online()))
            .collect()
    }

    /// "Herkese" özetinde kişilerin kendi kopyalarına verdiği tepkiler.
    fn summary_reactions(&self, m: &ChatMessage) -> BTreeMap<String, String> {
        let mut all = m.reactions.clone();
        for id in &m.children {
            if let Some(c) = self.messages.iter().find(|x| &x.id == id) {
                for (who, e) in &c.reactions {
                    if *who != self.cfg.id {
                        all.insert(who.clone(), e.clone());
                    }
                }
            }
        }
        all
    }

    /// "Herkese" sohbetindeki özet mesajın durumu (kopyaların durumundan).
    fn broadcast_status(&self, m: &ChatMessage) -> String {
        let kids: Vec<&ChatMessage> = m
            .children
            .iter()
            .filter_map(|id| self.messages.iter().find(|x| &x.id == id))
            .collect();
        let total = m.children.len();
        let done = kids.iter().filter(|k| k.state == MsgState::Done).count();
        let failed = kids
            .iter()
            .filter(|k| matches!(k.state, MsgState::Failed(_)))
            .count();
        let queued = kids.iter().filter(|k| k.state == MsgState::Queued).count();
        let read = kids.iter().filter(|k| k.read).count();
        if failed > 0 {
            format!(
                "{done}/{total} teslim edildi · {failed} başarısız (kişinin sohbetinden tekrar deneyin)"
            )
        } else if queued > 0 {
            format!("{done}/{total} teslim edildi · 🕓 {queued} kişi çevrimiçi olunca gidecek")
        } else if done < total {
            format!("gönderiliyor… {done}/{total}")
        } else if read == total {
            format!("✔✔ {total} kişinin hepsi okudu")
        } else {
            format!("✔ {total} kişinin hepsine teslim edildi")
        }
    }

    fn send_text(&mut self) {
        let text = self.input.trim_end().to_string();
        if text.trim().is_empty() {
            return;
        }
        if let Some((chat, id)) = self.editing.take()
            && Some(&chat) == self.selected.as_ref()
        {
            self.apply_edit(&id, text);
            self.input.clear();
            return;
        }
        if self.selected.as_deref() == Some(BROADCAST) {
            let targets = self.broadcast_targets();
            if targets.is_empty() {
                self.show_toast("Henüz eşleşmiş cihaz yok");
                return;
            }
            let mut children = Vec::new();
            for (peer_id, _, _) in targets {
                let id = new_id();
                self.push_message(ChatMessage::new(
                    id.clone(),
                    peer_id,
                    true,
                    Body::Text(text.clone()),
                    MsgState::Queued,
                ));
                self.dispatch(&id);
                children.push(id);
            }
            self.push_message(ChatMessage {
                children,
                ..ChatMessage::new(
                    new_id(),
                    BROADCAST.into(),
                    true,
                    Body::Text(text),
                    MsgState::Done,
                )
            });
            self.input.clear();
            return;
        }
        let Some(peer_id) = self.target_peer() else {
            return;
        };
        let id = new_id();
        let reply_to = match self.replying.take() {
            Some((p, r)) if p == peer_id => Some(r),
            _ => None,
        };
        self.push_message(ChatMessage {
            reply_to,
            ..ChatMessage::new(
                id.clone(),
                peer_id,
                true,
                Body::Text(text),
                MsgState::Queued,
            )
        });
        self.dispatch(&id);
        self.input.clear();
    }

    /// Dosyaları ve/veya klasörleri gönderir.
    fn send_paths(&mut self, paths: Vec<PathBuf>) {
        let broadcast = self.selected.as_deref() == Some(BROADCAST);
        let targets: Vec<String> = if broadcast {
            let t: Vec<_> = self
                .broadcast_targets()
                .into_iter()
                .map(|(id, _, _)| id)
                .collect();
            if t.is_empty() {
                self.show_toast("Henüz eşleşmiş cihaz yok");
                return;
            }
            t
        } else {
            match self.target_peer() {
                Some(t) => vec![t],
                None => return,
            }
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
            let body = Body::File {
                name,
                // Klasörün boyutu tarama bitince belli olur
                size: if is_dir { 0 } else { meta.len() },
                path: Some(path.clone()),
                is_dir,
                file_count: if is_dir { 0 } else { 1 },
            };
            let mut children = Vec::new();
            for peer_id in &targets {
                let id = new_id();
                self.push_message(ChatMessage::new(
                    id.clone(),
                    peer_id.clone(),
                    true,
                    body.clone(),
                    MsgState::Queued,
                ));
                self.dispatch(&id);
                children.push(id);
            }
            if broadcast {
                self.push_message(ChatMessage {
                    children,
                    ..ChatMessage::new(new_id(), BROADCAST.into(), true, body, MsgState::Done)
                });
            }
        }
    }

    /// Başarısız mesajı yeniden gönderir; dosya/klasörde kaldığı yerden devam eder.
    fn retry(&mut self, msg_id: &str) {
        self.dispatch(msg_id);
    }

    /* ------------------------------ Çizim ------------------------------ */

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add(egui::Image::new(&self.logo).fit_to_exact_size(egui::vec2(26.0, 26.0)));
            ui.label(RichText::new("LanChat").strong().size(18.0));
            ui.separator();
            ui.label(RichText::new(format!("Adın: {}", self.cfg.name)).weak());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.menu_button("👤 Hesaplar", |ui| self.accounts_menu(ui));
                if ui.button("⚙ Ayarlar").clicked() {
                    self.settings = Some(Settings {
                        theme: self.cfg.theme,
                        zoom: self.cfg.zoom,
                        speed: self.cfg.speed_limit,
                        close_to_tray: self.cfg.close_to_tray,
                        notifications: self.cfg.notifications,
                        notification_sound: self.cfg.notification_sound,
                        windows_sound: self.cfg.windows_sound,
                        autostart: desktop::autostart_enabled(&self.cfg.id),
                        has_password: self.profile.has_password(),
                        pw_current: String::new(),
                        pw_new: String::new(),
                        pw_repeat: String::new(),
                        pw_error: None,
                        name: self.cfg.name.clone(),
                        download_dir: self.cfg.download_dir.clone(),
                        manual_ips: self.cfg.manual_ips.clone(),
                        trusted: self.cfg.trusted.clone(),
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

    /// Eşleşmiş, çevrimiçi ve aynı platformdaki cihazlar arasında bizimkinden
    /// yeni LanChat'i olan (en yeni sürümlü) cihaz: (kimlik, ad, adres, sürüm)
    fn update_offer(&self) -> Option<(String, String, SocketAddr, String)> {
        let mut best: Option<(String, String, SocketAddr, String)> = None;
        for p in &self.peers {
            let (Some(addr), true) = (p.addr, p.online()) else {
                continue;
            };
            if !(self.cfg.is_trusted(&p.id) || p.local) {
                continue;
            }
            let Some((ver, plat)) = self.peer_versions.get(&p.id) else {
                continue;
            };
            if *plat != protocol::platform() || !protocol::version_newer(ver, protocol::APP_VERSION)
            {
                continue;
            }
            if best
                .as_ref()
                .is_none_or(|b| protocol::version_newer(ver, &b.3))
            {
                best = Some((p.id.clone(), p.name.clone(), addr, ver.clone()));
            }
        }
        best
    }

    fn update_bar(&mut self, ui: &mut egui::Ui) {
        let accent = Color32::from_rgb(220, 38, 38);
        match &self.update {
            UpdateState::Idle => {
                let Some((id, name, addr, version)) = self.update_offer() else {
                    return;
                };
                if self.update_dismissed.as_deref() == Some(version.as_str()) {
                    return;
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "⬆ LanChat'in yeni sürümü ({version}) {name} bilgisayarında var. \
                             Sizdeki: {}",
                            protocol::APP_VERSION
                        ))
                        .color(accent),
                    );
                    if ui.button("Güncelle").clicked() {
                        self.start_update(id, name, addr);
                    }
                    if ui.button("Daha sonra").clicked() {
                        self.update_dismissed = Some(version);
                    }
                });
            }
            UpdateState::Downloading { from, done, total } => {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(format!("⬆ Yeni sürüm {from} bilgisayarından indiriliyor…"));
                    let frac = if *total > 0 {
                        *done as f32 / *total as f32
                    } else {
                        0.0
                    };
                    ui.add(
                        egui::ProgressBar::new(frac)
                            .desired_width(180.0)
                            .show_percentage(),
                    );
                });
            }
            UpdateState::Ready { version, .. } => {
                let version = version.clone();
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "✔ LanChat {version} indirildi ve doğrulandı. Yeniden başlatınca kurulacak."
                        ))
                        .color(Color32::from_rgb(22, 163, 74)),
                    );
                    if ui.button("Şimdi yeniden başlat").clicked() {
                        self.install_update(ui.ctx());
                    }
                });
            }
            UpdateState::Failed(err) => {
                let err = err.clone();
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("Güncelleme başarısız: {err}")).color(accent));
                    if ui.button("Tamam").clicked() {
                        self.update = UpdateState::Idle;
                    }
                });
            }
        }
    }

    fn start_update(&mut self, peer_id: String, name: String, addr: SocketAddr) {
        let (Some(net), Ok(exe)) = (&self.net, std::env::current_exe()) else {
            self.update = UpdateState::Failed("Programın yeri bulunamadı".into());
            return;
        };
        net.request_update(addr, peer_id, update::download_path(&exe));
        self.update = UpdateState::Downloading {
            from: name,
            done: 0,
            total: 0,
        };
    }

    fn install_update(&mut self, ctx: &egui::Context) {
        let UpdateState::Ready { path, .. } = &self.update else {
            return;
        };
        let path = path.clone();
        self.save_history();
        let result = std::env::current_exe().and_then(|exe| {
            update::install(&exe, &path)?;
            update::restart(&exe, &self.cfg.id)
        });
        match result {
            Ok(()) => {
                self.quitting = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Err(e) => {
                self.update = UpdateState::Failed(format!("Kurulamadı: {e}"));
            }
        }
    }

    fn accounts_menu(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new(format!("Bu pencere: {}", self.cfg.name)).strong());
        ui.separator();
        let others: Vec<_> = storage::list_profiles(&self.root_dir)
            .into_iter()
            .filter(|p| p.id != self.cfg.id)
            .collect();
        if others.is_empty() {
            ui.label(RichText::new("Başka hesap yok").weak());
        }
        for p in others {
            let label = if p.in_use {
                format!("🗗 {}  (açık)", p.name)
            } else {
                format!("🗗 {}", p.name)
            };
            let resp = ui
                .add_enabled(!p.in_use, egui::Button::new(label))
                .on_hover_text("Bu hesabı yeni bir pencerede aç")
                .on_disabled_hover_text("Bu hesap başka bir pencerede zaten açık");
            if resp.clicked() {
                ui.close();
                if let Err(e) = open_account_window(&p.id) {
                    self.show_toast(format!("Yeni pencere açılamadı: {e}"));
                }
            }
        }
        ui.separator();
        if ui.button("➕ Yeni hesap…").clicked() {
            ui.close();
            self.new_account = Some(NewAccount {
                name: String::new(),
                error: None,
                focused: false,
            });
        }
        ui.separator();
        if ui
            .button("💾 Bu hesabı yedekle…")
            .on_hover_text(
                "Hesabın ayarlarını, anahtarını ve sohbet geçmişini tek bir dosyaya kaydeder. \
                 Gelen dosyalar yedeğe girmez.",
            )
            .clicked()
        {
            ui.close();
            self.backup_account();
        }
        if ui
            .button("📥 Yedekten hesap geri yükle…")
            .on_hover_text(
                "Başka bir bilgisayarda ya da daha önce alınmış yedeği bu bilgisayara ekler",
            )
            .clicked()
        {
            ui.close();
            self.restore_account();
        }
    }

    fn backup_account(&mut self) {
        let Some(dest) = rfd::FileDialog::new()
            .set_title("Hesap yedeğini kaydet")
            .set_file_name(backup::backup_file_name(&self.cfg.name))
            .add_filter("LanChat yedeği", &[backup::EXTENSION])
            .save_file()
        else {
            return;
        };
        self.save_history();
        let _ = self.cfg.save(&self.data_dir);
        match backup::backup_profile(&self.data_dir, &dest) {
            Ok(()) => self.show_toast(if self.profile.has_password() {
                "Yedek kaydedildi (sohbet geçmişi ve anahtar, hesap şifrenizle şifreli)"
            } else {
                "Yedek kaydedildi. Hesabın gizli anahtarını içerir; güvenli bir yerde saklayın."
            }),
            Err(e) => self.show_toast(format!("Yedek kaydedilemedi: {e}")),
        }
    }

    fn restore_account(&mut self) {
        let Some(src) = rfd::FileDialog::new()
            .set_title("Geri yüklenecek yedeği seçin")
            .add_filter("LanChat yedeği", &[backup::EXTENSION, "zip"])
            .pick_file()
        else {
            return;
        };
        match backup::restore_profile(&self.root_dir, &src) {
            Ok(p) => {
                self.show_toast(format!(
                    "“{}” hesabı geri yüklendi; yeni pencerede açılıyor",
                    p.name
                ));
                if let Err(e) = open_account_window(&p.id) {
                    self.show_toast(format!("Yeni pencere açılamadı: {e}"));
                }
            }
            Err(e) => self.show_toast(e),
        }
    }

    /// Seçili sohbeti HTML ya da metin dosyası olarak kaydeder.
    fn export_chat(&mut self, chat: &str, title: &str) {
        let Some(dest) = rfd::FileDialog::new()
            .set_title("Sohbeti dışa aktar")
            .set_file_name(backup::export_file_name(title))
            .add_filter("Web sayfası", &["html"])
            .add_filter("Metin dosyası", &["txt"])
            .save_file()
        else {
            return;
        };
        let msgs: Vec<&ChatMessage> = self.messages.iter().filter(|m| m.peer_id == chat).collect();
        let peer_name = if chat == BROADCAST { "?" } else { title };
        match backup::export_chat(&dest, title, &self.cfg.name, peer_name, &msgs) {
            Ok(path) => {
                self.show_toast(format!("Sohbet kaydedildi ({} mesaj)", msgs.len()));
                reveal_path(&path);
            }
            Err(e) => self.show_toast(format!("Kaydedilemedi: {e}")),
        }
    }

    fn new_account_window(&mut self, ctx: &egui::Context) {
        let Some(na) = &mut self.new_account else {
            return;
        };
        let mut create = false;
        let mut close = false;
        egui::Window::new("Yeni hesap")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label("Yeni hesap ayrı bir pencerede açılır; kendi adı, sohbet geçmişi ve indirme klasörü olur.");
                ui.add_space(6.0);
                let r = ui.add(
                    TextEdit::singleline(&mut na.name)
                        .hint_text("Hesap adı (ör. “Kamil – Kişisel”)")
                        .char_limit(40)
                        .desired_width(f32::INFINITY),
                );
                if !na.focused {
                    r.request_focus();
                    na.focused = true;
                }
                if (r.has_focus() || r.lost_focus()) && ui.input(|i| i.key_pressed(Key::Enter)) {
                    create = true;
                }
                if let Some(e) = &na.error {
                    ui.colored_label(Color32::from_rgb(220, 38, 38), e);
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(RichText::new("Oluştur ve aç").strong()).clicked() {
                        create = true;
                    }
                    if ui.button("İptal").clicked() {
                        close = true;
                    }
                });
            });
        if create {
            let name = na.name.trim().to_string();
            if name.is_empty() {
                na.error = Some("Bir ad yazın".into());
                return;
            }
            let result = storage::create_profile(&self.root_dir, &name)
                .map_err(|e| e.to_string())
                .and_then(|p| open_account_window(&p.id).map_err(|e| e.to_string()));
            match result {
                Ok(()) => {
                    self.new_account = None;
                    self.show_toast(format!("“{name}” hesabı yeni pencerede açılıyor…"));
                }
                Err(e) => na.error = Some(e),
            }
        } else if close {
            self.new_account = None;
        }
    }

    fn pairing_windows(&mut self, ctx: &egui::Context) {
        // Bizim başlattığımız eşleştirme
        if let Some(op) = &mut self.out_pair {
            let mut confirm = false;
            let mut close = false;
            egui::Window::new("🔑 Eşleştirme")
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.set_width(380.0);
                    ui.label(format!("“{}” ile eşleştiriliyor", op.name));
                    ui.add_space(8.0);
                    match (&op.code, &op.error) {
                        (_, Some(e)) => {
                            ui.colored_label(Color32::from_rgb(220, 38, 38), e);
                        }
                        (None, None) => {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label("Bağlanılıyor…");
                            });
                        }
                        (Some(code), None) => {
                            ui.vertical_centered(|ui| {
                                ui.label(RichText::new(code).monospace().size(34.0).strong());
                            });
                            ui.add_space(6.0);
                            ui.label(
                                "Diğer bilgisayarın ekranında da bir kod göründü. \
                                 İki kod aynıysa burada ve orada onaylayın.",
                            );
                            ui.add_space(6.0);
                            match op.remote {
                                Some((true, _)) => {
                                    ui.colored_label(
                                        Color32::from_rgb(34, 197, 94),
                                        "✔ Karşı taraf onayladı",
                                    );
                                }
                                _ => {
                                    ui.horizontal(|ui| {
                                        ui.spinner();
                                        ui.label(
                                            RichText::new("Karşı tarafın onayı bekleniyor…").weak(),
                                        );
                                    });
                                }
                            }
                        }
                    }
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if op.error.is_none() && op.code.is_some() && !op.confirmed {
                            if ui
                                .button(RichText::new("Kodlar aynı — Onayla").strong())
                                .clicked()
                            {
                                confirm = true;
                            }
                            if ui.button("Kodlar farklı — İptal").clicked() {
                                close = true;
                            }
                        } else if op.confirmed && op.error.is_none() {
                            ui.label(RichText::new("Onayladınız.").weak());
                            if ui.button("Kapat").clicked() {
                                close = true;
                            }
                        } else if ui.button("Kapat").clicked() {
                            close = true;
                        }
                    });
                });
            if confirm {
                op.confirmed = true;
                self.finish_out_pair();
            } else if close {
                self.out_pair = None;
            }
        }

        // Karşı tarafın başlattığı eşleştirme
        if let Some(ip) = &self.in_pair {
            let mut decision: Option<bool> = None;
            if ip.since.elapsed() > Duration::from_secs(175) {
                decision = Some(false);
            }
            egui::Window::new("🔑 Eşleştirme isteği")
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.set_width(380.0);
                    ui.label(format!(
                        "“{}” ({}) sizinle eşleşmek istiyor.",
                        ip.from.name,
                        ip.addr.ip()
                    ));
                    ui.add_space(8.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new(&ip.code).monospace().size(34.0).strong());
                    });
                    ui.add_space(6.0);
                    ui.label(
                        "Diğer bilgisayarın ekranındaki kodla aynıysa onaylayın. \
                         Aynı değilse ya da bu isteği siz başlatmadıysanız reddedin.",
                    );
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui
                            .button(RichText::new("Kodlar aynı — Onayla").strong())
                            .clicked()
                        {
                            decision = Some(true);
                        }
                        if ui.button("Reddet").clicked() {
                            decision = Some(false);
                        }
                    });
                });
            if let Some(accept) = decision {
                let ip = self.in_pair.take().unwrap();
                let _ = ip.reply.send(accept);
                if accept {
                    self.cfg.add_trusted(&ip.from.id, &ip.from.name, &ip.key);
                    self.save_trusted();
                    self.selected = Some(ip.from.id.clone());
                    self.show_toast(format!(
                        "🔒 “{}” ile eşleşildi; artık konuşmalarınız şifreli",
                        ip.from.name
                    ));
                }
            }
        }
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
            } else {
                let selected = self.selected.as_deref() == Some(BROADCAST);
                let n = self.broadcast_targets().len();
                let resp = ui
                    .add_sized(
                        [ui.available_width(), 28.0],
                        egui::Button::selectable(selected, RichText::new("📢 Herkese").size(15.0)),
                    )
                    .on_hover_text(format!(
                        "Mesaj ve dosyaları eşleşmiş tüm cihazlara gönder ({n} kişi; çevrimdışı olanlara açılınca gider)"
                    ));
                if resp.clicked() {
                    self.selected = Some(BROADCAST.to_string());
                    ui.memory_mut(|m| m.request_focus(Id::unique(INPUT_ID)));
                }
                ui.separator();
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
                if self.cfg.is_trusted(&p.id) || p.local {
                    text.append(
                        "  🔒",
                        0.0,
                        egui::TextFormat::simple(FontId::proportional(12.0), Color32::GRAY),
                    );
                }
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
            ui.label(
                RichText::new(format!("LanChat {}", protocol::APP_VERSION))
                    .small()
                    .weak(),
            );
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
                .on_hover_text(format!(
                    "Mesaj portu: {}\nLanChat sürümü: {}",
                    net.tcp_port(),
                    protocol::APP_VERSION
                ));
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
        let current = self.selected.clone();
        let mut cancel_reply = false;
        if let Some((p, r)) = &self.replying
            && Some(p) == current.as_ref()
        {
            Frame::new()
                .fill(ui.visuals().faint_bg_color)
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("↩").strong());
                        ui.label(RichText::new(&r.author).strong().small());
                        ui.add(
                            egui::Label::new(RichText::new(&r.preview).small().weak()).truncate(),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui
                                .small_button("✖")
                                .on_hover_text("Yanıtlamayı iptal et")
                                .clicked()
                            {
                                cancel_reply = true;
                            }
                        });
                    });
                });
            ui.add_space(4.0);
        }
        if cancel_reply {
            self.replying = None;
        }
        let mut cancel_edit = false;
        if let Some((chat, _)) = &self.editing
            && Some(chat) == current.as_ref()
        {
            Frame::new()
                .fill(ui.visuals().faint_bg_color)
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("✏ Mesajı düzenliyorsunuz").strong().small());
                        ui.label(RichText::new("Enter: kaydet · Esc: vazgeç").small().weak());
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui
                                .small_button("✖")
                                .on_hover_text("Düzenlemeyi iptal et")
                                .clicked()
                            {
                                cancel_edit = true;
                            }
                        });
                    });
                });
            ui.add_space(4.0);
            if ui.input(|i| i.key_pressed(Key::Escape)) {
                cancel_edit = true;
            }
        }
        if cancel_edit {
            self.editing = None;
            self.input.clear();
        }
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
                ui.separator();
                if ui
                    .button("📋  Panodakini gönder   Ctrl+Shift+V")
                    .on_hover_text(
                        "Dosya Gezgini'nde kopyaladığınız dosyaları, ekran görüntüsünü ya da \
                         kopyaladığınız metni gönderir",
                    )
                    .clicked()
                {
                    ui.close();
                    let ctx = ui.ctx().clone();
                    self.send_clipboard(&ctx);
                }
            })
            .response
            .on_hover_text("Dosya ya da klasör gönder (pencereye sürükleyip de bırakabilirsiniz)");
            if let Some(paths) = picked {
                self.send_paths(paths);
            }
            if current.as_deref() != Some(BROADCAST)
                && ui
                    .button(RichText::new("📳").size(18.0))
                    .on_hover_text("Titreşim gönder: karşı tarafın penceresi öne gelir ve sallanır")
                    .clicked()
            {
                let ctx = ui.ctx().clone();
                self.send_nudge(&ctx);
            }
            let send_w = 80.0;
            let edit = TextEdit::multiline(&mut self.input)
                .id(input_id)
                .desired_rows(2)
                .desired_width(ui.available_width() - send_w - 8.0)
                .hint_text("Mesaj yazın…  (Enter: gönder, Shift+Enter: yeni satır)");
            if ui.add(edit).changed() && !self.input.trim().is_empty() && self.editing.is_none() {
                self.notify_typing();
            }
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

        let is_broadcast = peer_id == BROADCAST;
        let targets = if is_broadcast {
            self.broadcast_targets()
        } else {
            Vec::new()
        };
        let (name, online) = if is_broadcast {
            ("📢 Herkese".to_string(), targets.iter().any(|t| t.2))
        } else {
            self.peer(&peer_id)
                .map(|p| (p.name.clone(), p.online()))
                .unwrap_or_else(|| ("?".into(), false))
        };
        let paired = is_broadcast || self.is_paired(&peer_id);
        let local = self.peer(&peer_id).is_some_and(|p| p.local);

        // Arama (Ctrl+F ile de açılır)
        if ui.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::F)) {
            self.search = Some((peer_id.clone(), String::new()));
            self.search_focus = true;
        }
        let query: Option<String> = match &self.search {
            Some((p, q)) if *p == peer_id && !q.trim().is_empty() => Some(fold_tr(q.trim())),
            _ => None,
        };
        let match_count = query.as_ref().map(|q| {
            self.messages
                .iter()
                .filter(|m| m.peer_id == peer_id && message_matches(m, q))
                .count()
        });

        let mut pair_clicked = false;
        let mut toggle_search = false;
        let mut export_clicked = false;
        let mut wake_clicked = false;
        let macs_known = self.peer(&peer_id).is_some_and(|p| !p.macs.is_empty());
        egui::Panel::top("chat-header").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(&name).strong().size(16.0));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.menu_button("⋯", |ui| {
                        if ui.button("💾 Sohbeti dışa aktar…").on_hover_text("Bu sohbeti bir web sayfası (.html) ya da metin dosyası (.txt) olarak kaydeder").clicked() {
                            ui.close();
                            export_clicked = true;
                        }
                    });
                    if ui
                        .small_button("🔍")
                        .on_hover_text("Bu sohbette ara (Ctrl+F)")
                        .clicked()
                    {
                        toggle_search = true;
                    }
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                if is_broadcast {
                    let names: Vec<&str> = targets.iter().map(|(_, n, _)| n.as_str()).collect();
                    let on = targets.iter().filter(|t| t.2).count();
                    let text = if names.is_empty() {
                        "henüz eşleşmiş cihaz yok".to_string()
                    } else if on == names.len() {
                        format!("{} kişiye gider", names.len())
                    } else {
                        format!(
                            "{} kişiye gider ({} çevrimdışı: açılınca alacak)",
                            names.len(),
                            names.len() - on
                        )
                    };
                    ui.label(RichText::new(text).small().weak())
                        .on_hover_text(names.join(", "));
                    return;
                }
                let (txt, col) = if online {
                    ("çevrimiçi", Color32::from_rgb(34, 197, 94))
                } else {
                    ("çevrimdışı", Color32::GRAY)
                };
                ui.label(RichText::new(txt).color(col).small());
                if !online && paired && !local {
                    let b = ui
                        .add_enabled(macs_known, egui::Button::new("⏰ Uyandır").small())
                        .on_hover_text(
                            "Kapalı ya da uykudaki bilgisayarı ağ üzerinden açmayı dener \
                             (Wake-on-LAN). Bilgisayarın BIOS'unda ve ağ kartı ayarlarında \
                             açık olmalıdır; genellikle kablolu bağlantıda çalışır.",
                        )
                        .on_disabled_hover_text(
                            "Bu bilgisayarın ağ kartı adresi henüz bilinmiyor. LanChat'in yeni \
                             sürümüyle bir kez çevrimiçi olunca öğrenilir.",
                        );
                    if b.clicked() {
                        wake_clicked = true;
                    }
                }
                if self
                    .typing
                    .get(&peer_id)
                    .is_some_and(|t| *t > Instant::now())
                {
                    ui.label(RichText::new("yazıyor…").italics().small().weak());
                }
                if paired {
                    let hover = if local {
                        "Bu bilgisayardaki diğer hesabınız; mesajlar ve dosyalar şifreli gider"
                    } else {
                        "Eşleşilmiş cihaz; mesajlar ve dosyalar uçtan uca şifreli gider"
                    };
                    ui.label(
                        RichText::new("🔒 şifreli")
                            .small()
                            .color(Color32::from_rgb(34, 197, 94)),
                    )
                    .on_hover_text(hover);
                }
                    });
                });
            });
            // Arama satırı
            let mut close_search = false;
            if let Some((p, q)) = &mut self.search
                && *p == peer_id
            {
                ui.horizontal(|ui| {
                    let r = ui.add(
                        TextEdit::singleline(q)
                            .id(Id::unique(SEARCH_ID))
                            .hint_text("Bu sohbette ara…")
                            .desired_width(260.0),
                    );
                    if self.search_focus {
                        r.request_focus();
                        self.search_focus = false;
                    }
                    if r.has_focus() && ui.input(|i| i.key_pressed(Key::Escape)) {
                        close_search = true;
                    }
                    if let Some(n) = match_count {
                        let text = if n == 0 {
                            "sonuç yok".to_string()
                        } else {
                            format!("{n} sonuç")
                        };
                        ui.label(RichText::new(text).small().weak());
                    }
                    if ui
                        .small_button("✖")
                        .on_hover_text("Aramayı kapat (Esc)")
                        .clicked()
                    {
                        close_search = true;
                    }
                });
            }
            if close_search {
                self.search = None;
            }
            ui.add_space(4.0);
        });
        if wake_clicked {
            self.wake_peer(&peer_id);
        }
        if export_clicked {
            self.export_chat(&peer_id, &name);
        }
        if toggle_search {
            if self.search.as_ref().is_some_and(|(p, _)| *p == peer_id) {
                self.search = None;
            } else {
                self.search = Some((peer_id.clone(), String::new()));
                self.search_focus = true;
            }
        }

        if paired {
            egui::Panel::bottom("composer").show(ui, |ui| self.composer(ui));
        } else {
            egui::Panel::bottom("pair-banner").show(ui, |ui| {
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("🔑").size(22.0));
                    // Düğmeye yer bırakıp metni kalan genişlikte sar
                    let button_w = 120.0;
                    let text_w = (ui.available_width() - button_w - 16.0).max(120.0);
                    ui.allocate_ui(egui::vec2(text_w, 0.0), |ui| {
                        ui.vertical(|ui| {
                            ui.label(RichText::new("Bu kişiyle henüz eşleşmediniz").strong());
                            ui.add(
                                egui::Label::new(
                                    RichText::new(
                                        "Mesaj ve dosya göndermek için bir kez eşleştirin. İki ekranda \
                                         aynı kod görünecek; kodlar aynıysa iki tarafta da onaylayın.",
                                    )
                                    .small()
                                    .weak(),
                                )
                                .wrap(),
                            );
                        });
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add_enabled(
                                online,
                                egui::Button::new(RichText::new("🔑 Eşleştir").strong()),
                            )
                            .on_disabled_hover_text("Kişi çevrimiçi olduğunda eşleştirebilirsiniz")
                            .clicked()
                        {
                            pair_clicked = true;
                        }
                    });
                });
                ui.add_space(10.0);
            });
        }
        if pair_clicked {
            self.start_pairing(&peer_id);
        }

        let mut actions: Vec<Action> = Vec::new();
        let mut wanted_thumbs: Vec<String> = Vec::new();
        let names: HashMap<String, String> = self
            .peers
            .iter()
            .map(|p| (p.id.clone(), p.name.clone()))
            .chain([(self.cfg.id.clone(), "Siz".to_string())])
            .collect();
        egui::CentralPanel::default().show(ui, |ui| {
            ScrollArea::vertical()
                .stick_to_bottom(true)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add_space(8.0);
                    let mut last_day = String::new();
                    let msgs: Vec<&ChatMessage> = self
                        .messages
                        .iter()
                        .filter(|m| m.peer_id == peer_id)
                        .filter(|m| query.as_ref().is_none_or(|q| message_matches(m, q)))
                        .collect();
                    if msgs.is_empty() && query.is_some() {
                        ui.vertical_centered(|ui| {
                            ui.add_space(40.0);
                            ui.label(RichText::new("Aramayla eşleşen mesaj yok.").weak());
                        });
                    } else if msgs.is_empty() {
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
                        if matches!(m.body, Body::Nudge) {
                            nudge_line(ui, m, &name);
                            continue;
                        }
                        let status = is_broadcast.then(|| self.broadcast_status(m));
                        let thumb = match self.thumbs.get(&m.id) {
                            Some(Thumb::Ready(t)) => Some(t),
                            _ => None,
                        };
                        if let Body::File { name, path: Some(_), is_dir: false, .. } = &m.body
                            && m.state == MsgState::Done
                            && !m.deleted
                            && is_image(name)
                            && !self.thumbs.contains_key(&m.id)
                            && ui.is_rect_visible(egui::Rect::from_min_size(
                                ui.cursor().min,
                                egui::vec2(10.0, 300.0),
                            ))
                        {
                            wanted_thumbs.push(m.id.clone());
                        }
                        let reactions = if is_broadcast {
                            Cow::Owned(self.summary_reactions(m))
                        } else {
                            Cow::Borrowed(&m.reactions)
                        };
                        let ctx = BubbleCtx {
                            status,
                            thumb,
                            reactions: &reactions,
                            my_id: &self.cfg.id,
                            names: &names,
                        };
                        message_bubble(ui, m, ctx, &mut actions);
                        ui.add_space(4.0);
                    }
                    ui.add_space(4.0);
                });
        });

        if !wanted_thumbs.is_empty() {
            let ctx = ui.ctx().clone();
            self.request_thumbs(&ctx, wanted_thumbs);
        }
        for a in actions {
            match a {
                Action::OpenFile(p) => open_path(&p),
                Action::DragOut(p) => self.drag_out = Some(p),
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
                Action::Reply(id) => self.start_reply(&id, &name),
                Action::DeleteLocal(id) => {
                    self.messages.retain(|m| m.id != id);
                    self.dirty = true;
                }
                Action::DeleteForAll(id) => self.delete_for_all(&id),
                Action::Edit(id) => {
                    self.start_edit(&id);
                    ui.memory_mut(|m| m.request_focus(Id::unique(INPUT_ID)));
                }
                Action::React(id, emoji) => self.react(&id, emoji),
            }
        }
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        let Some(s) = &mut self.settings else { return };
        let mut close = false;
        let mut apply = false;
        let mut pw_action: Option<PasswordAction> = None;
        let max_h = (ctx.content_rect().height() - 120.0).max(200.0);

        egui::Window::new("Ayarlar")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(420.0)
            .show(ctx, |ui| {
              ScrollArea::vertical().max_height(max_h).show(ui, |ui| {
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

                ui.label(RichText::new("Görünüm").strong());
                ui.horizontal(|ui| {
                    ui.radio_value(&mut s.theme, ThemeChoice::Sistem, "Windows'a uy");
                    ui.radio_value(&mut s.theme, ThemeChoice::Acik, "☀ Açık");
                    ui.radio_value(&mut s.theme, ThemeChoice::Koyu, "🌙 Koyu");
                });
                ui.horizontal(|ui| {
                    ui.label("Yazı boyutu:");
                    ui.add(
                        egui::Slider::new(&mut s.zoom, 0.5..=2.0)
                            .step_by(0.1)
                            .custom_formatter(|v, _| format!("%{:.0}", v * 100.0)),
                    );
                });
                ui.label(
                    RichText::new("Ctrl + artı / eksi ile de büyütüp küçültebilirsiniz (Ctrl+0: normal).")
                        .small()
                        .weak(),
                );
                ui.add_space(10.0);

                ui.label(RichText::new("Aktarım hızı").strong());
                ui.horizontal(|ui| {
                    ui.checkbox(&mut s.speed.enabled, "Gönderirken hızı sınırla:");
                    ui.add_enabled(
                        s.speed.enabled,
                        egui::DragValue::new(&mut s.speed.mb_per_sec)
                            .range(1..=2000)
                            .suffix(" MB/sn"),
                    );
                });
                ui.add_enabled_ui(s.speed.enabled, |ui| {
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut s.speed.only_hours, "Yalnızca şu saatler arasında:");
                        ui.add_enabled(
                            s.speed.only_hours,
                            egui::DragValue::new(&mut s.speed.from_hour)
                                .range(0..=23)
                                .suffix(":00"),
                        );
                        ui.label("–");
                        ui.add_enabled(
                            s.speed.only_hours,
                            egui::DragValue::new(&mut s.speed.to_hour)
                                .range(0..=23)
                                .suffix(":00"),
                        );
                    });
                });
                ui.label(
                    RichText::new("Bu bilgisayarın gönderdiği dosyalar için geçerlidir; büyük aktarımlar mesai saatlerinde ağı tıkamasın diye. Süren aktarımlara da hemen uygulanır.")
                        .small()
                        .weak(),
                );
                ui.add_space(10.0);

                if desktop::HAS_TRAY {
                    ui.label(RichText::new("Masaüstü").strong());
                    ui.checkbox(&mut s.autostart, "Windows açılınca bu hesabı arka planda başlat");
                    ui.checkbox(
                        &mut s.close_to_tray,
                        "Pencere kapatılınca saatin yanındaki simgede çalışmaya devam et",
                    );
                    ui.checkbox(&mut s.notifications, "Yeni mesaj ve dosyalarda bildirim göster");
                    ui.add_enabled_ui(s.notifications, |ui| {
                        ui.checkbox(&mut s.notification_sound, "Bildirim sesi çal");
                        ui.add_enabled_ui(s.notification_sound, |ui| {
                            ui.horizontal(|ui| {
                                ui.add_space(24.0);
                                ui.radio_value(&mut s.windows_sound, false, "LanChat (dıdıt dıt)");
                                ui.radio_value(&mut s.windows_sound, true, "Windows sesi");
                                if ui.button("▶ Dinle").clicked() {
                                    desktop::play_sound(sound_kind(s.windows_sound));
                                }
                            });
                        });
                    });
                    ui.add_space(10.0);
                }

                ui.label(RichText::new("Hesap şifresi").strong());
                let pw_field = |ui: &mut egui::Ui, text: &mut String, hint: &str| {
                    ui.add(TextEdit::singleline(text).password(true).hint_text(hint).desired_width(260.0));
                };
                if s.has_password {
                    ui.label(
                        RichText::new("🔒 Bu hesap şifreli. Açılırken şifre sorulur; sohbet geçmişi ve hesabın anahtarı bilgisayarda şifreli saklanır.")
                            .small()
                            .weak(),
                    );
                    pw_field(ui, &mut s.pw_current, "Mevcut şifre");
                    pw_field(ui, &mut s.pw_new, "Yeni şifre");
                    pw_field(ui, &mut s.pw_repeat, "Yeni şifre (tekrar)");
                    ui.horizontal(|ui| {
                        if ui.button("Şifreyi değiştir").clicked() {
                            if s.pw_current.is_empty() {
                                s.pw_error = Some("Mevcut şifreyi yazın".into());
                            } else if let Err(e) = validate_new_password(&s.pw_new, &s.pw_repeat) {
                                s.pw_error = Some(e);
                            } else {
                                pw_action = Some(PasswordAction::Change {
                                    current: s.pw_current.clone(),
                                    new: s.pw_new.clone(),
                                });
                            }
                        }
                        if ui.button("Şifreyi kaldır").clicked() {
                            if s.pw_current.is_empty() {
                                s.pw_error = Some("Şifreyi kaldırmak için mevcut şifreyi yazın".into());
                            } else {
                                pw_action = Some(PasswordAction::Remove { current: s.pw_current.clone() });
                            }
                        }
                    });
                } else {
                    ui.label(
                        RichText::new("İsteğe bağlı. Şifre koyarsanız bu hesap açılırken şifre sorulur; sohbet geçmişi ve hesabın anahtarı bilgisayarda şifreli saklanır.")
                            .small()
                            .weak(),
                    );
                    pw_field(ui, &mut s.pw_new, "Yeni şifre");
                    pw_field(ui, &mut s.pw_repeat, "Yeni şifre (tekrar)");
                    if ui.button("🔒 Şifre koy").clicked() {
                        match validate_new_password(&s.pw_new, &s.pw_repeat) {
                            Ok(()) => pw_action = Some(PasswordAction::Set(s.pw_new.clone())),
                            Err(e) => s.pw_error = Some(e),
                        }
                    }
                }
                if let Some(e) = &s.pw_error {
                    ui.colored_label(Color32::from_rgb(220, 38, 38), e);
                }
                ui.label(
                    RichText::new("Şifreyi unutursanız kurtarılamaz; hesap ancak sohbet geçmişi silinerek sıfırlanabilir. Gelen dosyalar indirme klasöründe şifresiz durur.")
                        .small()
                        .weak(),
                );
                ui.add_space(10.0);

                ui.label(RichText::new("Eşleşilmiş cihazlar").strong());
                ui.label(
                    RichText::new("Yalnızca bu cihazlar size mesaj ve dosya gönderebilir. Kaldırırsanız yeniden eşleştirmeniz gerekir.")
                        .small()
                        .weak(),
                );
                if s.trusted.is_empty() {
                    ui.label(RichText::new("Henüz eşleşilmiş cihaz yok").weak());
                }
                let mut remove_trusted = None;
                for (i, t) in s.trusted.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(format!("🔒 {}", t.name));
                        if ui.small_button("✖").on_hover_text("Eşleşmeyi kaldır").clicked() {
                            remove_trusted = Some(i);
                        }
                    });
                }
                if let Some(i) = remove_trusted {
                    s.trusted.remove(i);
                }
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
            });

        if let Some(action) = pw_action {
            self.apply_password_action(action);
            return;
        }

        if apply {
            let s = self.settings.take().unwrap();
            let name: String = s.name.trim().chars().take(40).collect();
            if !name.is_empty() {
                self.cfg.name = name;
            }
            self.cfg.download_dir = s.download_dir;
            self.cfg.manual_ips = s.manual_ips;
            self.cfg.trusted = s.trusted;
            self.cfg.speed_limit = s.speed;
            self.cfg.theme = s.theme;
            self.cfg.zoom = s.zoom;
            apply_appearance(ctx, &self.cfg);
            self.cfg.close_to_tray = s.close_to_tray;
            self.cfg.notifications = s.notifications;
            self.cfg.notification_sound = s.notification_sound;
            self.cfg.windows_sound = s.windows_sound;
            if desktop::HAS_TRAY
                && s.autostart != desktop::autostart_enabled(&self.cfg.id)
                && let Err(e) = desktop::set_autostart(&self.cfg.id, s.autostart)
            {
                self.show_toast(format!("Windows başlangıç ayarı değiştirilemedi: {e}"));
                return;
            }
            if let Some(net) = &self.net {
                net.set_speed_limit(self.cfg.speed_limit);
                net.set_trusted(self.cfg.trusted_map());
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

    fn apply_password_action(&mut self, action: PasswordAction) {
        let history = self.current_history();
        let (current, new) = match &action {
            PasswordAction::Set(new) => (None, Some(new.as_str())),
            PasswordAction::Change { current, new } => (Some(current.as_str()), Some(new.as_str())),
            PasswordAction::Remove { current } => (Some(current.as_str()), None),
        };
        let result = if current.is_some_and(|c| !self.profile.check_password(c)) {
            Err("Mevcut şifre yanlış".to_string())
        } else {
            self.profile
                .set_password(new, &history)
                .map_err(|e| format!("Şifre kaydedilemedi: {e}"))
        };
        let Some(s) = &mut self.settings else { return };
        match result {
            Ok(()) => {
                s.has_password = self.profile.has_password();
                s.pw_current.clear();
                s.pw_new.clear();
                s.pw_repeat.clear();
                s.pw_error = None;
                self.dirty = false;
                self.show_toast(match action {
                    PasswordAction::Set(_) => {
                        "🔒 Şifre kondu; hesap bundan sonra şifreyle açılacak"
                    }
                    PasswordAction::Change { .. } => "🔒 Şifre değiştirildi",
                    PasswordAction::Remove { .. } => "Şifre kaldırıldı; hesap şifresiz açılacak",
                });
            }
            Err(e) => s.pw_error = Some(e),
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
            let visible = Duration::from_millis(2500 + 40 * text.chars().count() as u64);
            if at.elapsed() > visible {
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
    OpenFile(PathBuf),
    /// Dosyayı pencereden dışarı sürükle
    DragOut(PathBuf),
    Retry(String),
    Cancel(String),
    Copy(String),
    Reply(String),
    DeleteLocal(String),
    DeleteForAll(String),
    Edit(String),
    React(String, &'static str),
}

/// Çevrimiçi görünüp bağlanılamayan kişiye sıradakileri yeniden deneme aralığı
const QUEUE_RETRY: Duration = Duration::from_secs(30);
/// Aynı kişiye art arda titreşim gönderilemeyecek süre
const NUDGE_COOLDOWN: Duration = Duration::from_secs(10);
/// Pencerenin sallanma süresi
const SHAKE_TIME: Duration = Duration::from_millis(700);

/// Mesaja verilebilecek tepkiler
const REACTIONS: [&str; 6] = ["👍", "❤", "😂", "😮", "😢", "🙏"];

/// Balonun çizilmesi için gereken ek bilgiler
struct BubbleCtx<'a> {
    /// "Herkese" sohbetindeki özet mesajlar için toplu teslim durumu
    status: Option<String>,
    thumb: Option<&'a egui::TextureHandle>,
    reactions: &'a BTreeMap<String, String>,
    my_id: &'a str,
    /// Kişi kimliği → görünen ad
    names: &'a HashMap<String, String>,
}

/// Silinen mesajın içeriğini temizler.
fn clear_message(m: &mut ChatMessage) {
    m.deleted = true;
    m.body = Body::Text(String::new());
    m.reply_to = None;
    m.note = None;
}

fn message_bubble(ui: &mut egui::Ui, m: &ChatMessage, bc: BubbleCtx, actions: &mut Vec<Action>) {
    let BubbleCtx {
        status,
        thumb,
        reactions,
        my_id,
        names,
    } = bc;
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
        // Sağ tık menüsü için balonun tamamı tıklanabilir; içindeki düğmeler önceliklidir
        let resp = ui
            .scope_builder(egui::UiBuilder::new().sense(egui::Sense::click()), |ui| {
        Frame::new()
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
                    if let Some(r) = &m.reply_to {
                        Frame::new()
                            .fill(ui.visuals().faint_bg_color.gamma_multiply(1.6))
                            .corner_radius(CornerRadius::same(6))
                            .inner_margin(Margin::symmetric(8, 4))
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new(&r.author)
                                        .small()
                                        .strong()
                                        .color(Color32::from_rgb(248, 113, 113)),
                                );
                                ui.add(
                                    egui::Label::new(RichText::new(&r.preview).small().weak())
                                        .truncate(),
                                );
                            });
                        ui.add_space(2.0);
                    }
                    match &m.body {
                        _ if m.deleted => {
                            ui.label(RichText::new("🚫 Bu mesaj silindi").italics().weak());
                        }
                        Body::Text(t) => {
                            ui.add(egui::Label::new(RichText::new(t).size(15.0)).wrap());
                        }
                        Body::File { .. } => file_body(ui, m, thumb, actions),
                        Body::Nudge => {}
                    }
                    if !reactions.is_empty() && !m.deleted {
                        reaction_chips(ui, m, reactions, my_id, names, actions);
                    }
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format_time(m.ts)).small().weak());
                        if m.edited && !m.deleted {
                            ui.label(RichText::new("düzenlendi").small().italics().weak());
                        }
                        if let Some(st) = &status {
                            let col = if st.starts_with('✔') {
                                Color32::from_rgb(34, 197, 94)
                            } else {
                                ui.visuals().weak_text_color()
                            };
                            ui.label(RichText::new(st).small().color(col));
                        } else if m.outgoing {
                            match &m.state {
                                MsgState::Queued => {
                                    ui.label(
                                        RichText::new("🕓 sırada")
                                            .small()
                                            .color(Color32::from_rgb(234, 179, 8)),
                                    )
                                    .on_hover_text(
                                        "Kişi şu an çevrimdışı. Çevrimiçi olunca kendiliğinden gönderilecek.",
                                    );
                                }
                                MsgState::Pending => {
                                    if matches!(m.body, Body::Text(_)) {
                                        ui.label(RichText::new("gönderiliyor…").small().weak());
                                    }
                                }
                                MsgState::Done if m.read => {
                                    ui.label(
                                        RichText::new("✔✔")
                                            .small()
                                            .color(Color32::from_rgb(56, 189, 248)),
                                    )
                                    .on_hover_text("Okundu");
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
            });
            })
            .response;

        let w = resp.rect.width();
        if prev_w.is_none_or(|p| (p - w).abs() > 0.5) {
            ui.data_mut(|d| d.insert_temp(width_id, w));
            ui.ctx().request_repaint();
        }

        let is_summary = m.peer_id == BROADCAST;
        resp.context_menu(|ui| {
            if !m.deleted && m.state == MsgState::Done {
                ui.horizontal(|ui| {
                    let mine = reactions.get(my_id).map(String::as_str);
                    for e in REACTIONS {
                        let b = egui::Button::selectable(mine == Some(e), RichText::new(e).size(18.0));
                        if ui.add(b).clicked() {
                            actions.push(Action::React(m.id.clone(), e));
                            ui.close();
                        }
                    }
                });
                ui.separator();
            }
            if m.outgoing
                && !m.deleted
                && matches!(m.body, Body::Text(_))
                && m.state == MsgState::Done
                && ui.button("✏ Düzenle").clicked()
            {
                actions.push(Action::Edit(m.id.clone()));
                ui.close();
            }
            if !m.deleted && !is_summary {
                if ui.button("↩ Yanıtla").clicked() {
                    actions.push(Action::Reply(m.id.clone()));
                    ui.close();
                }
                if let Body::Text(t) = &m.body
                    && ui.button("📋 Kopyala").clicked()
                {
                    actions.push(Action::Copy(t.clone()));
                    ui.close();
                }
                ui.separator();
            }
            if ui.button("🗑 Benden sil").clicked() {
                actions.push(Action::DeleteLocal(m.id.clone()));
                ui.close();
            }
            if m.outgoing
                && !m.deleted
                && !is_summary
                && m.state == MsgState::Done
                && ui
                    .button("🗑 Herkesten sil")
                    .on_hover_text("Karşı taraftan da silinir. Gönderilmiş dosyalar onun bilgisayarında kalır.")
                    .clicked()
            {
                actions.push(Action::DeleteForAll(m.id.clone()));
                ui.close();
            }
        });
    });
}

/// Titreşim: sohbetin ortasında küçük bir satır.
fn nudge_line(ui: &mut egui::Ui, m: &ChatMessage, peer_name: &str) {
    let text = if m.outgoing {
        "📳 Titreşim gönderdiniz".to_string()
    } else {
        format!("📳 {peer_name} titreşim gönderdi")
    };
    ui.vertical_centered(|ui| {
        ui.add_space(2.0);
        ui.add(
            egui::Label::new(
                RichText::new(format!("{text} · {}", format_time(m.ts)))
                    .small()
                    .italics()
                    .color(Color32::from_rgb(234, 88, 12)),
            )
            .wrap(),
        );
        ui.add_space(2.0);
    });
}

/// Balonun altındaki tepkiler: aynı emojiler sayıyla birlikte gruplanır.
fn reaction_chips(
    ui: &mut egui::Ui,
    m: &ChatMessage,
    reactions: &BTreeMap<String, String>,
    my_id: &str,
    names: &HashMap<String, String>,
    actions: &mut Vec<Action>,
) {
    let mut groups: Vec<(&str, Vec<&str>)> = Vec::new();
    for (who, e) in reactions {
        match groups.iter_mut().find(|(g, _)| g == e) {
            Some((_, v)) => v.push(who),
            None => groups.push((e, vec![who])),
        }
    }
    ui.add_space(2.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (emoji, who) in groups {
            let mine = who.contains(&my_id);
            let label = if who.len() > 1 {
                format!("{emoji} {}", who.len())
            } else {
                emoji.to_string()
            };
            let who_names: Vec<&str> = who
                .iter()
                .map(|w| names.get(*w).map(String::as_str).unwrap_or("?"))
                .collect();
            let resp = ui
                .add(
                    egui::Button::selectable(mine, RichText::new(label).size(16.0))
                        .corner_radius(CornerRadius::same(10)),
                )
                .on_hover_text(who_names.join(", "));
            // Kendi tepkimize tıklayınca kaldırılır, başkasınınkine tıklayınca aynısı eklenir
            if resp.clicked()
                && let Some(e) = REACTIONS.iter().find(|r| **r == emoji)
            {
                actions.push(Action::React(m.id.clone(), e));
            }
        }
    });
}

fn file_body(
    ui: &mut egui::Ui,
    m: &ChatMessage,
    thumb: Option<&egui::TextureHandle>,
    actions: &mut Vec<Action>,
) {
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
    if let (Some(tex), Some(p)) = (thumb, path) {
        let resp = ui
            .add(
                egui::Image::new(tex)
                    .max_size(egui::vec2(280.0, 220.0))
                    .corner_radius(8)
                    .sense(egui::Sense::click()),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text("Açmak için tıklayın");
        if resp.clicked() {
            actions.push(Action::OpenFile(p.clone()));
        }
        ui.add_space(2.0);
    }
    let scanning =
        is_dir && m.outgoing && size == 0 && *file_count == 0 && m.state == MsgState::Pending;

    let row = ui.horizontal(|ui| {
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
    // Teslim edilmiş dosya, satırından tutulup masaüstüne/klasöre sürüklenebilir
    if desktop::HAS_DRAG_OUT
        && m.state == MsgState::Done
        && let Some(p) = path
        && p.exists()
    {
        let drag = ui
            .interact(
                row.response.rect,
                Id::unique(("dosya-surukle", m.id.as_str())),
                egui::Sense::drag(),
            )
            .on_hover_cursor(egui::CursorIcon::Grab)
            .on_hover_text("Masaüstüne ya da bir klasöre sürükleyip bırakabilirsiniz");
        if drag.drag_started() {
            actions.push(Action::DragOut(p.clone()));
        }
    }

    match &m.state {
        MsgState::Queued => {
            ui.label(
                RichText::new("Kişi çevrimiçi olunca gönderilecek")
                    .small()
                    .weak(),
            );
        }
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

        self.desktop_logic(ctx);
        // Ctrl + artı/eksi ile değiştirilen büyüklüğü hatırla
        let zoom = ctx.zoom_factor();
        if (zoom - self.cfg.zoom).abs() > 0.01 {
            self.cfg.zoom = zoom;
            if let Some(s) = &mut self.settings {
                s.zoom = zoom;
            }
            let _ = self.cfg.save(&self.data_dir);
        }
        self.shake_logic(ctx);
        self.focused = !self.hidden && ctx.input(|i| i.viewport().focused.unwrap_or(true));
        if self.focused
            && let Some(sel) = &self.selected
        {
            self.unread.remove(sel);
        }
        let total: usize = self.unread.values().sum();
        let title = if total > 0 {
            format!("({total}) LanChat — {}", self.cfg.name)
        } else {
            format!("LanChat — {}", self.cfg.name)
        };
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }

        if let Some(tray) = &mut self.tray {
            let blinking = total > 0 && !self.focused;
            tray.set_blinking(blinking);
            tray.set_tooltip(&if total > 0 {
                format!("LanChat — {}\n{total} okunmamış mesaj", self.cfg.name)
            } else {
                format!("LanChat — {}", self.cfg.name)
            });
            if blinking {
                ctx.request_repaint_after(desktop::BLINK_INTERVAL / 2);
            }
        }

        self.send_read_receipts();
        if self.last_queue_check.elapsed() > Duration::from_secs(2) {
            self.last_queue_check = Instant::now();
            self.retry_queues();
        }
        self.receive_thumbs(ctx);
        if self.dirty && self.last_save.elapsed() > Duration::from_secs(2) {
            self.save_history();
        }
        // Çevrimiçi/çevrimdışı durumunu güncel tutmak için
        ctx.request_repaint_after(Duration::from_secs(1));
    }

    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        // Windows'un sürükle-bırak döngüsü fare bırakma olayını yuttuğu için
        // egui düğmeyi hâlâ basılı sanmasın
        if std::mem::take(&mut self.release_pointer) {
            let pos = raw_input
                .events
                .iter()
                .rev()
                .find_map(|e| match e {
                    egui::Event::PointerMoved(p) => Some(*p),
                    _ => None,
                })
                .unwrap_or_default();
            raw_input.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            });
            raw_input.events.push(egui::Event::PointerGone);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
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

        // Ctrl+V ile yapıştırılan resim (ör. ekran görüntüsü): önce önizleme
        let pasted = ctx.input(|i| {
            i.events.iter().rev().find_map(|e| match e {
                egui::Event::PasteImage(img) => Some((**img).clone()),
                _ => None,
            })
        });
        if let Some(img) = pasted
            && self.settings.is_none()
        {
            self.preview_paste(&ctx, img);
        }
        if self.settings.is_none()
            && ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::V))
        {
            self.send_clipboard(&ctx);
        }
        if self.paste_preview.is_some() {
            self.paste_preview_window(&ctx);
        }
        if self.send_choice.is_some() {
            self.send_choice_window(&ctx);
        }

        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(6.0);
            self.top_bar(ui);
            if let Some(err) = &self.startup_error {
                ui.colored_label(Color32::from_rgb(220, 38, 38), err);
            }
            self.update_bar(ui);
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

        if let Some(path) = self.drag_out.take() {
            desktop::drag_out(frame, &path);
            self.release_pointer = true;
            ctx.request_repaint();
        }

        self.settings_window(&ctx);
        self.new_account_window(&ctx);
        self.pairing_windows(&ctx);
        self.overlays(&ctx);
    }

    fn on_exit(&mut self) {
        self.save_history();
        if let Some(net) = &self.net {
            net.shutdown();
        }
    }
}

/* ------------------------------ Yardımcılar ------------------------------ */

fn sound_kind(windows_sound: bool) -> desktop::SoundKind {
    if windows_sound {
        desktop::SoundKind::Windows
    } else {
        desktop::SoundKind::LanChat
    }
}

/// Üst çubuktaki küçük logo: küçük boyutlar için çizilmiş sade sürüm.
pub fn small_logo_texture(ctx: &egui::Context) -> egui::TextureHandle {
    load_png_texture(
        ctx,
        "lanchat-logo-small",
        include_bytes!("../assets/icon/lanchat-small-48.png"),
    )
}

/// Büyük gösterimler için harfli tam sürüm.
pub fn large_logo_texture(ctx: &egui::Context) -> egui::TextureHandle {
    load_png_texture(
        ctx,
        "lanchat-logo-large",
        include_bytes!("../assets/icon/lanchat-128.png"),
    )
}

fn load_png_texture(ctx: &egui::Context, name: &str, png: &[u8]) -> egui::TextureHandle {
    let icon = eframe::icon_data::from_png_bytes(png).expect("gömülü logo geçerli bir PNG");
    let image = egui::ColorImage::from_rgba_unmultiplied(
        [icon.width as usize, icon.height as usize],
        &icon.rgba,
    );
    ctx.load_texture(name, image, egui::TextureOptions::LINEAR)
}

/// Verilen hesabı yeni bir LanChat penceresinde (ayrı işlem olarak) açar.
pub fn open_account_window(profile_id: &str) -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    std::process::Command::new(exe)
        .arg("--hesap-id")
        .arg(profile_id)
        .spawn()
        .map(|_| ())
}

/// Hesabın tema ve yazı boyutu ayarını uygular.
pub fn apply_appearance(ctx: &egui::Context, cfg: &Config) {
    ctx.set_theme(match cfg.theme {
        ThemeChoice::Sistem => egui::ThemePreference::System,
        ThemeChoice::Acik => egui::ThemePreference::Light,
        ThemeChoice::Koyu => egui::ThemePreference::Dark,
    });
    ctx.set_zoom_factor(cfg.zoom.clamp(0.5, 2.0));
}

pub fn setup_style(ctx: &egui::Context) {
    // Hem koyu hem açık tema için (yalnızca etkin olana değil)
    ctx.all_styles_mut(|style| {
        // Metinler fareyle seçilmez: mesaja sağ tıklanınca menü açılsın
        // (kopyalama menüdeki "Kopyala" ile)
        style.interaction.selectable_labels = false;
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

/// Türkçe karakterlere ve büyük/küçük harfe duyarsız karşılaştırma için sadeleştirir
/// ("Şifre" → "sifre", "İSTANBUL" → "istanbul").
fn fold_tr(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'İ' | 'I' | 'ı' => 'i',
            'Ş' | 'ş' => 's',
            'Ğ' | 'ğ' => 'g',
            'Ü' | 'ü' => 'u',
            'Ö' | 'ö' => 'o',
            'Ç' | 'ç' => 'c',
            c => c,
        })
        .flat_map(char::to_lowercase)
        .collect()
}

/// Mesaj, sadeleştirilmiş aramayla eşleşiyor mu? (metin, dosya adı, alıntı)
fn message_matches(m: &ChatMessage, folded_query: &str) -> bool {
    if m.deleted {
        return false;
    }
    let body = match &m.body {
        Body::Text(t) => t.as_str(),
        Body::File { name, .. } => name.as_str(),
        Body::Nudge => "titreşim",
    };
    fold_tr(body).contains(folded_query)
        || m.reply_to
            .as_ref()
            .is_some_and(|r| fold_tr(&r.preview).contains(folded_query))
}

fn validate_new_password(new: &str, repeat: &str) -> Result<(), String> {
    if new.chars().count() < 4 {
        Err("Şifre en az 4 karakter olmalı".into())
    } else if new != repeat {
        Err("İki şifre aynı değil".into())
    } else {
        Ok(())
    }
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
    fn image_detection_and_thumbnail_size() {
        assert!(is_image("Tatil.JPG"));
        assert!(is_image("ekran.png"));
        assert!(!is_image("rapor.pdf"));
        assert!(!is_image("png"));

        // Büyük bir resim, oranı korunarak en fazla 560x440'a küçültülür
        let dir = std::env::temp_dir().join(format!("lanchat-thumb-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("buyuk.png");
        image::RgbaImage::from_pixel(2000, 1000, image::Rgba([200, 30, 30, 255]))
            .save(&path)
            .unwrap();
        let t = image::open(&path).unwrap().thumbnail(560, 440);
        assert_eq!((t.width(), t.height()), (560, 280));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn search_is_turkish_and_case_insensitive() {
        assert_eq!(fold_tr("İSTANBUL Şişli ığdır"), "istanbul sisli igdir");
        let msg = |t: &str| {
            ChatMessage::new(
                "1".into(),
                "p".into(),
                true,
                Body::Text(t.into()),
                MsgState::Done,
            )
        };
        assert!(message_matches(&msg("Yeni Şifre geldi"), &fold_tr("sifre")));
        assert!(message_matches(&msg("aksam toplantisi"), &fold_tr("Akşam")));
        assert!(!message_matches(&msg("merhaba"), &fold_tr("dosya")));
        let file = ChatMessage::new(
            "2".into(),
            "p".into(),
            false,
            Body::File {
                name: "Teklif Şubat.pdf".into(),
                size: 1,
                path: None,
                is_dir: false,
                file_count: 1,
            },
            MsgState::Done,
        );
        assert!(message_matches(&file, &fold_tr("şubat")));
        let mut deleted = msg("gizli");
        deleted.deleted = true;
        assert!(!message_matches(&deleted, "gizli"));
    }

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
