//! Masaüstü arayüzü (egui).

use crate::net::{self, Net, NetEvent, NetOptions, format_bytes};
use crate::protocol::{self, ReplyRef};
use crate::secure::PublicKey;
use crate::storage::{
    self, Body, ChatMessage, Config, History, KnownPeer, MsgState, Profile, TrustedPeer,
};
use chrono::{Local, TimeZone};
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Frame, Id, Key, Layout, Margin, Modifiers,
    RichText, ScrollArea, Stroke, TextEdit,
};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

const ONLINE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_HISTORY: usize = 5000;
const INPUT_ID: &str = "composer-input";
/// "Herkese" sohbetinin kimliği (gerçek bir kişi değil)
const BROADCAST: &str = "__herkese__";

struct Peer {
    id: String,
    name: String,
    addr: Option<SocketAddr>,
    last_seen: Option<Instant>,
    /// Aynı bilgisayardaki (bu Windows kullanıcısının) başka bir hesap
    local: bool,
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

struct Settings {
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
    /// Resim önizlemeleri (mesaj kimliğine göre)
    thumbs: HashMap<String, Thumb>,
    thumb_tx: Sender<(String, Option<egui::ColorImage>)>,
    thumb_rx: Receiver<(String, Option<egui::ColorImage>)>,
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
    pub fn new(ctx: &egui::Context, root_dir: PathBuf, profile: Profile) -> Self {
        let data_dir = profile.dir.clone();
        let cfg = profile.cfg.clone();
        let history = profile.load_history();
        let _ = storage::set_last_profile(&root_dir, &cfg.id);

        let local_registry = Some(storage::online_dir(&root_dir));
        let logo = small_logo_texture(ctx);
        let (thumb_tx, thumb_rx) = std::sync::mpsc::channel();
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
            thumbs: HashMap::new(),
            thumb_tx,
            thumb_rx,
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

    fn current_history(&self) -> History {
        History {
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
        let Some(m) = self
            .messages
            .iter_mut()
            .find(|m| m.id == msg_id && m.outgoing)
        else {
            return;
        };
        clear_message(m);
        let peer_id = m.peer_id.clone();
        self.dirty = true;
        let addr = self.peer(&peer_id).and_then(|p| p.addr);
        if let (Some(net), Some(addr), true) = (&self.net, addr, self.is_paired(&peer_id)) {
            net.send_delete(addr, peer_id, msg_id.to_string());
            self.show_toast("Mesaj iki taraftan da silindi");
        } else {
            self.show_toast(
                "Mesaj sizden silindi; karşı taraf çevrimiçi olmadığı için onda kalabilir",
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

    fn target_addr(&mut self) -> Option<(String, SocketAddr)> {
        let Some(peer_id) = self.selected.clone() else {
            self.show_toast("Önce soldan bir kişi seçin");
            return None;
        };
        if !self.is_paired(&peer_id) {
            self.show_toast("Önce bu kişiyle eşleşmelisiniz: üstteki “🔑 Eşleştir”e basın");
            return None;
        }
        match self.peer(&peer_id).and_then(|p| p.addr) {
            Some(addr) => Some((peer_id, addr)),
            None => {
                self.show_toast("Bu kişinin adresi bilinmiyor");
                None
            }
        }
    }

    /// "Herkese" gönderiminde alıcılar: çevrimiçi ve eşleşmiş herkes.
    fn broadcast_targets(&self) -> Vec<(String, String, SocketAddr)> {
        self.peers
            .iter()
            .filter(|p| p.online() && (p.local || self.cfg.is_trusted(&p.id)))
            .filter_map(|p| Some((p.id.clone(), p.name.clone(), p.addr?)))
            .collect()
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
        let read = kids.iter().filter(|k| k.read).count();
        if failed > 0 {
            format!(
                "{done}/{total} teslim edildi · {failed} başarısız (kişinin sohbetinden tekrar deneyin)"
            )
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
        let Some(net) = self.net.clone() else { return };
        if self.selected.as_deref() == Some(BROADCAST) {
            let targets = self.broadcast_targets();
            if targets.is_empty() {
                self.show_toast("Şu an çevrimiçi eşleşmiş cihaz yok");
                return;
            }
            let mut children = Vec::new();
            for (peer_id, _, addr) in targets {
                let id = new_id();
                self.push_message(ChatMessage::new(
                    id.clone(),
                    peer_id.clone(),
                    true,
                    Body::Text(text.clone()),
                    MsgState::Pending,
                ));
                net.send_reply(addr, peer_id, id.clone(), text.clone(), None);
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
        let Some((peer_id, addr)) = self.target_addr() else {
            return;
        };
        let id = new_id();
        let reply_to = match self.replying.take() {
            Some((p, r)) if p == peer_id => Some(r),
            _ => None,
        };
        self.push_message(ChatMessage {
            reply_to: reply_to.clone(),
            ..ChatMessage::new(
                id.clone(),
                peer_id.clone(),
                true,
                Body::Text(text.clone()),
                MsgState::Pending,
            )
        });
        net.send_reply(addr, peer_id, id, text, reply_to);
        self.input.clear();
    }

    /// Dosyaları ve/veya klasörleri gönderir.
    fn send_paths(&mut self, paths: Vec<PathBuf>) {
        let Some(net) = self.net.clone() else { return };
        let broadcast = self.selected.as_deref() == Some(BROADCAST);
        let targets: Vec<(String, SocketAddr)> = if broadcast {
            let t: Vec<_> = self
                .broadcast_targets()
                .into_iter()
                .map(|(id, _, addr)| (id, addr))
                .collect();
            if t.is_empty() {
                self.show_toast("Şu an çevrimiçi eşleşmiş cihaz yok");
                return;
            }
            t
        } else {
            match self.target_addr() {
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
            for (peer_id, addr) in &targets {
                let id = new_id();
                self.push_message(ChatMessage::new(
                    id.clone(),
                    peer_id.clone(),
                    true,
                    body.clone(),
                    MsgState::Pending,
                ));
                net.send_path(*addr, peer_id.clone(), id.clone(), path.clone());
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
            Body::Text(t) => net.send_reply(addr, m.peer_id, m.id, t, m.reply_to),
            Body::File { path: Some(p), .. } => net.send_path(addr, m.peer_id, m.id, p),
            Body::File { path: None, .. } => {}
        }
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
                        "Mesaj ve dosyaları çevrimiçi tüm eşleşmiş cihazlara gönder (şu an {n} kişi)"
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
            if ui.add(edit).changed() && !self.input.trim().is_empty() {
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
            ("📢 Herkese".to_string(), !targets.is_empty())
        } else {
            self.peer(&peer_id)
                .map(|p| (p.name.clone(), p.online()))
                .unwrap_or_else(|| ("?".into(), false))
        };
        let paired = is_broadcast || self.is_paired(&peer_id);
        let local = self.peer(&peer_id).is_some_and(|p| p.local);

        let mut pair_clicked = false;
        egui::Panel::top("chat-header").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(&name).strong().size(16.0));
                if is_broadcast {
                    let names: Vec<&str> = targets.iter().map(|(_, n, _)| n.as_str()).collect();
                    let text = if names.is_empty() {
                        "şu an çevrimiçi eşleşmiş cihaz yok".to_string()
                    } else {
                        format!("{} çevrimiçi kişiye gider", names.len())
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
            ui.add_space(4.0);
        });

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
                        message_bubble(ui, m, status, thumb, &mut actions);
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
            if let Some(net) = &self.net {
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
    Retry(String),
    Cancel(String),
    Copy(String),
    Reply(String),
    DeleteLocal(String),
    DeleteForAll(String),
}

/// Silinen mesajın içeriğini temizler.
fn clear_message(m: &mut ChatMessage) {
    m.deleted = true;
    m.body = Body::Text(String::new());
    m.reply_to = None;
    m.note = None;
}

/// `status`: "Herkese" sohbetindeki özet mesajlar için toplu teslim durumu.
fn message_bubble(
    ui: &mut egui::Ui,
    m: &ChatMessage,
    status: Option<String>,
    thumb: Option<&egui::TextureHandle>,
    actions: &mut Vec<Action>,
) {
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
                    }
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format_time(m.ts)).small().weak());
                        if let Some(st) = &status {
                            let col = if st.starts_with('✔') {
                                Color32::from_rgb(34, 197, 94)
                            } else {
                                ui.visuals().weak_text_color()
                            };
                            ui.label(RichText::new(st).small().color(col));
                        } else if m.outgoing {
                            match &m.state {
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
            format!("({total}) LanChat — {}", self.cfg.name)
        } else {
            format!("LanChat — {}", self.cfg.name)
        };
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }

        self.send_read_receipts();
        self.receive_thumbs(ctx);
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

pub fn setup_style(ctx: &egui::Context) {
    ctx.global_style_mut(|style| {
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
