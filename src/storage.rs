//! Ayarlar ve sohbet geçmişinin diske kaydedilmesi.

use crate::secure::{Identity, PublicKey};
use crate::vault::{self, VaultError, VaultKey};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    pub id: String,
    pub name: String,
    pub download_dir: PathBuf,
    #[serde(default)]
    pub manual_ips: Vec<String>,
    /// Eşleşilmiş cihazlar
    #[serde(default)]
    pub trusted: Vec<TrustedPeer>,
    /// Gönderme hızı sınırı
    #[serde(default)]
    pub speed_limit: crate::net::SpeedLimit,
    /// Pencere kapatılınca sistem tepsisinde çalışmaya devam et (Windows)
    #[serde(default = "yes")]
    pub close_to_tray: bool,
    /// Yeni mesaj ve dosyalarda Windows bildirimi göster
    #[serde(default = "yes")]
    pub notifications: bool,
    /// Bildirimle birlikte ses çal
    #[serde(default = "yes")]
    pub notification_sound: bool,
    /// LanChat'in kendi sesi yerine Windows'un bildirim sesini kullan
    #[serde(default)]
    pub windows_sound: bool,
    /// Görünüm: sistem / açık / koyu
    #[serde(default)]
    pub theme: ThemeChoice,
    /// Yazı ve arayüz büyüklüğü (1.0 = normal)
    #[serde(default = "normal_zoom")]
    pub zoom: f32,
    /// "Tepside çalışmaya devam ediyor" bilgisi gösterildi mi?
    #[serde(default)]
    pub tray_hint_shown: bool,
}

fn yes() -> bool {
    true
}

fn normal_zoom() -> f32 {
    1.0
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq)]
pub enum ThemeChoice {
    /// Windows'un ayarına uy
    #[default]
    Sistem,
    Acik,
    Koyu,
}

const SHOW_REQUEST: &str = "goster";

/// Hesap başka bir pencerede (belki tepside gizli) açıksa o pencereden öne
/// gelmesini ister.
pub fn request_show(profile_dir: &Path) {
    let _ = fs::write(profile_dir.join(SHOW_REQUEST), b"");
}

/// Açık olan (ya da açılacak) hesaba "bu dosyaları gönder" isteği bırakır
/// (Dosya Gezgini → Gönder → LanChat).
pub fn request_send(profile_dir: &Path, paths: &[PathBuf]) -> io::Result<()> {
    let name = format!("gonder-{}.json", uuid::Uuid::new_v4().simple());
    let tmp = profile_dir.join(format!("{name}.tmp"));
    fs::write(&tmp, serde_json::to_vec(paths)?)?;
    fs::rename(tmp, profile_dir.join(name))
}

/// Bekleyen "gönder" isteklerindeki dosyalar (istekler silinir).
pub fn take_send_requests(profile_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(profile_dir) else {
        return out;
    };
    for e in rd.filter_map(Result::ok) {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with("gonder-") && name.ends_with(".json") {
            if let Ok(paths) = fs::read(e.path())
                .ok()
                .and_then(|b| serde_json::from_slice::<Vec<PathBuf>>(&b).ok())
                .ok_or(())
            {
                out.extend(paths);
            }
            let _ = fs::remove_file(e.path());
        }
    }
    out
}

/// Bu hesap için "öne gel" isteği var mı? (varsa siler)
pub fn take_show_request(profile_dir: &Path) -> bool {
    fs::remove_file(profile_dir.join(SHOW_REQUEST)).is_ok()
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TrustedPeer {
    pub id: String,
    pub name: String,
    /// Açık anahtar (hex)
    pub public_key: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum Body {
    Text(String),
    /// Dosya ya da klasör aktarımı
    File {
        name: String,
        /// Toplam bayt (klasörde tüm dosyaların toplamı)
        size: u64,
        /// Gönderende kaynak, alıcıda kaydedildiği yol
        path: Option<PathBuf>,
        #[serde(default)]
        is_dir: bool,
        #[serde(default)]
        file_count: u64,
    },
    /// Titreşim (MSN'deki gibi); sohbette ortada küçük bir satır olarak görünür
    Nudge,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum MsgState {
    /// Kişi çevrimdışı: çevrimiçi olunca kendiliğinden gönderilecek
    Queued,
    Pending,
    Done,
    Failed(String),
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ChatMessage {
    pub id: String,
    pub peer_id: String,
    pub outgoing: bool,
    /// Unix zaman damgası (saniye)
    pub ts: i64,
    pub body: Body,
    pub state: MsgState,
    /// Ek bilgi (ör. atlanan dosyalar)
    #[serde(default)]
    pub note: Option<String>,
    /// Gönderdiğimiz mesajı karşı taraf okudu mu?
    #[serde(default)]
    pub read: bool,
    /// Gelen mesaj için "okundu" bilgisini gönderdik mi?
    #[serde(default)]
    pub read_sent: bool,
    /// Bu mesaj başka bir mesaja yanıtsa, alıntı
    #[serde(default)]
    pub reply_to: Option<crate::protocol::ReplyRef>,
    /// Herkesten silindi (içerik temizlenir, "Bu mesaj silindi" gösterilir)
    #[serde(default)]
    pub deleted: bool,
    /// "Herkese" gönderilen mesajın kişilere giden kopyaları (mesaj kimlikleri)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<String>,
    /// Gönderildikten sonra düzeltildi mi?
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub edited: bool,
    /// Tepkiler: kişi kimliği (bizim için kendi hesap kimliğimiz) → emoji
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reactions: BTreeMap<String, String>,
    /// Bir grup mesajının bir üyeye giden kopyasıysa grubun kimliği
    /// (bu kopyalar kişiyle olan sohbette görünmez)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Grup mesajının tüm üyelerdeki ortak kimliği
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gmid: Option<String>,
    /// Gruptan gelen mesajı yazan kişi
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Aktarılan bayt (yalnızca aktarım sürerken anlamlı)
    #[serde(skip)]
    pub progress: u64,
    #[serde(skip)]
    pub speed: Speed,
}

impl ChatMessage {
    pub fn new(id: String, peer_id: String, outgoing: bool, body: Body, state: MsgState) -> Self {
        Self {
            id,
            peer_id,
            outgoing,
            ts: chrono::Local::now().timestamp(),
            body,
            state,
            note: None,
            read: false,
            read_sent: false,
            reply_to: None,
            deleted: false,
            children: Vec::new(),
            edited: false,
            reactions: BTreeMap::new(),
            group: None,
            gmid: None,
            author: None,
            progress: 0,
            speed: Speed::default(),
        }
    }
}

/// Aktarım hızının (bayt/sn) yumuşatılmış ölçümü.
#[derive(Clone, Debug, Default)]
pub struct Speed {
    pub bytes_per_sec: f64,
    sample: Option<(Instant, u64)>,
}

impl Speed {
    pub fn update(&mut self, done: u64) {
        let now = Instant::now();
        match self.sample {
            None => self.sample = Some((now, done)),
            Some((t0, b0)) => {
                let dt = now.duration_since(t0).as_secs_f64();
                if dt >= 1.0 {
                    let inst = done.saturating_sub(b0) as f64 / dt;
                    self.bytes_per_sec = if self.bytes_per_sec == 0.0 {
                        inst
                    } else {
                        0.7 * self.bytes_per_sec + 0.3 * inst
                    };
                    self.sample = Some((now, done));
                }
            }
        }
    }

    pub fn reset(&mut self) {
        *self = Speed::default();
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct KnownPeer {
    pub id: String,
    pub name: String,
    pub addr: Option<SocketAddr>,
    /// Uzaktan uyandırma için ağ kartı adresleri
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub macs: Vec<String>,
}

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct History {
    pub peers: Vec<KnownPeer>,
    pub messages: Vec<ChatMessage>,
    /// Çevrimdışı kişilere iletilmeyi bekleyen bildirimler (düzeltme, tepki, silme)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outbox: Vec<Outgoing>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<Group>,
}

/// Grup sohbeti
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Group {
    /// "grup-" ile başlar
    pub id: String,
    pub name: String,
    /// Tüm üyeler (biz dahil)
    pub members: Vec<crate::protocol::Member>,
    /// Bilginin sürümü (ms); daha yenisi gelirse güncellenir
    pub rev: i64,
    /// Gruptan çıktık: gelen mesajlar yok sayılır
    #[serde(default)]
    pub left: bool,
}

impl Group {
    /// Grup bilgisi, mesajla birlikte gönderilmek üzere
    pub fn to_ref(&self, gmid: &str) -> crate::protocol::GroupRef {
        crate::protocol::GroupRef {
            id: self.id.clone(),
            name: self.name.clone(),
            members: self.members.clone(),
            rev: self.rev,
            gmid: gmid.to_string(),
        }
    }

    pub fn is_group_id(id: &str) -> bool {
        id.starts_with("grup-")
    }
}

/// Bir kişiye gönderilecek kısa bildirim
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum Signal {
    Delete {
        msg_id: String,
    },
    Edit {
        msg_id: String,
        text: String,
    },
    React {
        msg_id: String,
        emoji: Option<String>,
    },
    /// Titreşim (sıraya konmaz: geç gelen titreşimin anlamı yok)
    Nudge,
}

impl Signal {
    /// Bildirimin ilgili olduğu mesaj
    pub fn msg_id(&self) -> Option<&str> {
        match self {
            Signal::Delete { msg_id }
            | Signal::Edit { msg_id, .. }
            | Signal::React { msg_id, .. } => Some(msg_id),
            Signal::Nudge => None,
        }
    }
}

/// Sırada bekleyen bildirim
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Outgoing {
    pub peer_id: String,
    pub signal: Signal,
}

impl History {
    /// Bildirimi sıraya koyar. Aynı mesaj için bekleyen eski bildirimin yerini alır
    /// (son düzeltme/tepki yeterli); silme, o mesajla ilgili her şeyin yerini alır.
    pub fn queue_signal(outbox: &mut Vec<Outgoing>, peer_id: &str, signal: Signal) {
        if matches!(signal, Signal::Nudge) {
            return;
        }
        let id = signal.msg_id().map(str::to_owned);
        let deleting = matches!(signal, Signal::Delete { .. });
        outbox.retain(|o| {
            if o.peer_id != peer_id || o.signal.msg_id() != id.as_deref() {
                return true;
            }
            !(deleting || std::mem::discriminant(&o.signal) == std::mem::discriminant(&signal))
        });
        outbox.push(Outgoing {
            peer_id: peer_id.to_string(),
            signal,
        });
    }
}

/// Bu Windows kullanıcısının LanChat kök veri klasörü (tüm hesaplar burada).
pub fn root_dir() -> PathBuf {
    if let Ok(d) = std::env::var("LANCHAT_DATA_DIR") {
        return PathBuf::from(d);
    }
    directories::ProjectDirs::from("com", "LanChat", "LanChat")
        .map(|p| p.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("lanchat-data"))
}

/// Bir hesabın varsayılan indirme klasörü: `İndirilenler/LanChat (ad)`
pub fn default_download_dir(account: &str) -> PathBuf {
    downloads_base().join(format!(
        "LanChat ({})",
        crate::protocol::sanitize_file_name(account)
    ))
}

fn downloads_base() -> PathBuf {
    directories::UserDirs::new()
        .and_then(|u| {
            u.download_dir()
                .map(Path::to_path_buf)
                .or_else(|| Some(u.home_dir().to_path_buf()))
        })
        .unwrap_or_else(|| PathBuf::from("."))
}

fn default_name() -> String {
    let host = gethostname::gethostname()
        .to_string_lossy()
        .trim()
        .to_string();
    if host.is_empty() {
        "Bilgisayarım".into()
    } else {
        host
    }
}

/// JSON'u önce geçici dosyaya yazıp sonra yerine taşır (yarım dosya kalmasın).
fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
    fs::rename(tmp, path)
}

impl Config {
    fn new(name: &str, download_dir: PathBuf) -> Config {
        Config {
            id: uuid::Uuid::new_v4().simple().to_string(),
            name: name.to_string(),
            download_dir,
            manual_ips: Vec::new(),
            trusted: Vec::new(),
            speed_limit: Default::default(),
            close_to_tray: true,
            notifications: true,
            notification_sound: true,
            theme: ThemeChoice::Sistem,
            zoom: 1.0,
            windows_sound: false,
            tray_hint_shown: false,
        }
    }

    pub fn trusted_map(&self) -> HashMap<String, PublicKey> {
        self.trusted
            .iter()
            .filter_map(|t| Some((t.id.clone(), crate::secure::from_hex(&t.public_key)?)))
            .collect()
    }

    pub fn is_trusted(&self, id: &str) -> bool {
        self.trusted.iter().any(|t| t.id == id)
    }

    /// Eşleştirilen cihazı ekler (aynı kimlik varsa anahtarını yeniler).
    pub fn add_trusted(&mut self, id: &str, name: &str, key: &PublicKey) {
        self.trusted.retain(|t| t.id != id);
        self.trusted.push(TrustedPeer {
            id: id.to_string(),
            name: name.to_string(),
            public_key: crate::secure::to_hex(key),
        });
    }

    pub fn load(dir: &Path) -> io::Result<Config> {
        let bytes = fs::read(dir.join("config.json"))?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub fn save(&self, dir: &Path) -> io::Result<()> {
        write_json(&dir.join("config.json"), self)
    }
}

/* ------------------------------ Hesaplar (profiller) ------------------------------ */
//
// <kök>/profiles/<hesap kimliği>/config.json, history.json, lock
// <kök>/online/<hesap kimliği>.json   (aynı bilgisayardaki açık hesaplar; bkz. net.rs)
// <kök>/last_profile                   (en son açılan hesap)

/// Hesap seçme ekranında gösterilecek bilgi.
#[derive(Clone, Debug)]
pub struct ProfileInfo {
    pub id: String,
    pub name: String,
    pub dir: PathBuf,
    /// Başka bir LanChat penceresinde açık mı?
    pub in_use: bool,
    /// Açılışta şifre soruluyor mu?
    pub has_password: bool,
}

/// Başka pencerelerin açmasına karşı kilitlenmiş ama henüz açılmamış hesap.
/// Şifreli hesapta anahtar ve geçmiş, şifre girilene kadar çözülmez.
#[derive(Debug)]
pub struct LockedProfile {
    pub dir: PathBuf,
    pub cfg: Config,
    lock: fs::File,
}

/// Açılmış hesap. Kilit, bu değer yaşadığı sürece tutulur;
/// böylece aynı hesap iki pencerede birden açılamaz.
#[derive(Debug)]
pub struct Profile {
    pub dir: PathBuf,
    pub cfg: Config,
    /// Hesabın şifreleme anahtarı
    pub identity: Identity,
    /// Hesap şifreliyse, şifreden türetilmiş anahtar
    vault: Option<VaultKey>,
    _lock: fs::File,
}

const KEY_FILE: &str = "key.json";
const HISTORY_FILE: &str = "history.json";

fn enc_name(name: &str) -> String {
    format!("{name}.enc")
}

/// Bu klasördeki hesabın şifresi var mı?
pub fn has_password(dir: &Path) -> bool {
    dir.join(enc_name(KEY_FILE)).exists()
}

/// Dosyayı önce geçici adla yazıp sonra yerine taşır.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
}

/// Şifresiz kopyayı silmeden önce üzerine sıfır yazar (en iyi çabayla).
fn wipe(path: &Path) {
    if let Ok(meta) = fs::metadata(path) {
        let _ = fs::write(path, vec![0u8; meta.len() as usize]);
        let _ = fs::remove_file(path);
    }
}

#[derive(Debug)]
pub enum ProfileError {
    InUse,
    Io(io::Error),
}

impl std::fmt::Display for ProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProfileError::InUse => write!(f, "Bu hesap başka bir LanChat penceresinde zaten açık"),
            ProfileError::Io(e) => write!(f, "Hesap açılamadı: {e}"),
        }
    }
}

pub fn profiles_dir(root: &Path) -> PathBuf {
    root.join("profiles")
}

pub fn online_dir(root: &Path) -> PathBuf {
    root.join("online")
}

fn lock_file(dir: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("lock"))
}

fn is_locked(dir: &Path) -> bool {
    lock_file(dir).is_ok_and(|f| matches!(f.try_lock(), Err(fs::TryLockError::WouldBlock)))
}

/// Eski sürümlerde ayarlar doğrudan kök klasördeydi; bunları ilk hesaba taşır.
fn migrate_legacy(root: &Path) {
    let legacy = root.join("config.json");
    let Ok(cfg) = Config::load(root) else { return };
    let dir = profiles_dir(root).join(&cfg.id);
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    for f in ["config.json", "history.json"] {
        let src = root.join(f);
        if src.exists() {
            let _ = fs::rename(&src, dir.join(f));
        }
    }
    if !legacy.exists() {
        let _ = set_last_profile(root, &cfg.id);
    }
}

/// Bu Windows kullanıcısının tüm LanChat hesapları (ada göre sıralı).
pub fn list_profiles(root: &Path) -> Vec<ProfileInfo> {
    migrate_legacy(root);
    let mut out: Vec<ProfileInfo> = fs::read_dir(profiles_dir(root))
        .map(|rd| {
            rd.filter_map(Result::ok)
                .filter_map(|e| {
                    let dir = e.path();
                    let cfg = Config::load(&dir).ok()?;
                    Some(ProfileInfo {
                        id: cfg.id,
                        name: cfg.name,
                        in_use: is_locked(&dir),
                        has_password: has_password(&dir),
                        dir,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort_by_key(|p| p.name.to_lowercase());
    out
}

/// Yeni bir hesap oluşturur. İlk hesap `İndirilenler/LanChat` klasörünü,
/// sonrakiler `İndirilenler/LanChat (ad)` klasörünü kullanır.
pub fn create_profile(root: &Path, name: &str) -> io::Result<ProfileInfo> {
    let name = name.trim();
    let name = if name.is_empty() {
        default_name()
    } else {
        name.chars().take(40).collect()
    };
    let first = list_profiles(root).is_empty();
    let downloads = if first {
        downloads_base().join("LanChat")
    } else {
        downloads_base().join(format!(
            "LanChat ({})",
            crate::protocol::sanitize_file_name(&name)
        ))
    };
    let cfg = Config::new(&name, downloads);
    let dir = profiles_dir(root).join(&cfg.id);
    cfg.save(&dir)?;
    Ok(ProfileInfo {
        id: cfg.id,
        name: cfg.name,
        dir,
        in_use: false,
        has_password: false,
    })
}

/// Hesabı başka pencerelere karşı kilitler. Açmak için `unlock` çağrılır.
pub fn open_profile(dir: &Path) -> Result<LockedProfile, ProfileError> {
    let cfg = Config::load(dir).map_err(ProfileError::Io)?;
    let lock = lock_file(dir).map_err(ProfileError::Io)?;
    match lock.try_lock() {
        Ok(()) => Ok(LockedProfile {
            dir: dir.to_path_buf(),
            cfg,
            lock,
        }),
        Err(fs::TryLockError::WouldBlock) => Err(ProfileError::InUse),
        Err(fs::TryLockError::Error(e)) => Err(ProfileError::Io(e)),
    }
}

#[derive(Debug)]
pub enum UnlockError {
    WrongPassword,
    Io(io::Error),
}

impl std::fmt::Display for UnlockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UnlockError::WrongPassword => write!(f, "Şifre yanlış"),
            UnlockError::Io(e) => write!(f, "Hesap açılamadı: {e}"),
        }
    }
}

impl LockedProfile {
    pub fn has_password(&self) -> bool {
        has_password(&self.dir)
    }

    /// Hesabı açar. Şifresiz hesapta `password` yok sayılır.
    /// Başarısızsa kilit bırakılmadan hesap geri verilir (yeniden denemek için).
    // Hata durumunda kilitli hesap geri verilir (kilidi bırakmadan yeniden denemek için);
    // bu nadir yol için büyük hata türü sorun değil.
    #[allow(clippy::result_large_err)]
    pub fn unlock(self, password: &str) -> Result<Profile, (LockedProfile, UnlockError)> {
        if !self.has_password() {
            return match Identity::load_or_create(&self.dir) {
                Ok(identity) => Ok(self.into_profile(identity, None)),
                Err(e) => Err((self, UnlockError::Io(e))),
            };
        }
        let data = match fs::read(self.dir.join(enc_name(KEY_FILE))) {
            Ok(d) => d,
            Err(e) => return Err((self, UnlockError::Io(e))),
        };
        let (key, plain) = match vault::unlock(password, &data) {
            Ok(r) => r,
            Err(VaultError::WrongPassword) => return Err((self, UnlockError::WrongPassword)),
            Err(e) => return Err((self, UnlockError::Io(io::Error::other(e)))),
        };
        match Identity::from_json(&plain) {
            Ok(identity) => Ok(self.into_profile(identity, Some(key))),
            Err(e) => Err((self, UnlockError::Io(e))),
        }
    }

    /// Şifresi unutulan hesabı sıfırlar: şifreli geçmiş ve anahtar silinir, yeni
    /// bir anahtar üretilir. Ad, ayarlar ve indirilen dosyalar korunur.
    pub fn reset_forgotten(self) -> io::Result<Profile> {
        let _ = fs::remove_file(self.dir.join(enc_name(KEY_FILE)));
        let _ = fs::remove_file(self.dir.join(enc_name(HISTORY_FILE)));
        let identity = Identity::load_or_create(&self.dir)?;
        Ok(self.into_profile(identity, None))
    }

    fn into_profile(self, identity: Identity, vault: Option<VaultKey>) -> Profile {
        Profile {
            dir: self.dir,
            cfg: self.cfg,
            identity,
            vault,
            _lock: self.lock,
        }
    }
}

impl Profile {
    pub fn has_password(&self) -> bool {
        self.vault.is_some()
    }

    pub fn load_history(&self) -> History {
        match &self.vault {
            None => History::load(&self.dir),
            Some(key) => fs::read(self.dir.join(enc_name(HISTORY_FILE)))
                .ok()
                .and_then(|d| key.decrypt(&d).ok())
                .map(|plain| History::from_bytes(&plain))
                .unwrap_or_default(),
        }
    }

    pub fn save_history(&self, h: &History) -> io::Result<()> {
        match &self.vault {
            None => h.save(&self.dir),
            Some(key) => write_atomic(
                &self.dir.join(enc_name(HISTORY_FILE)),
                &key.encrypt(&serde_json::to_vec(h)?),
            ),
        }
    }

    /// Mevcut şifre doğru mu? (Şifresiz hesapta her zaman doğru.)
    pub fn check_password(&self, password: &str) -> bool {
        self.vault.as_ref().is_none_or(|k| k.matches(password))
    }

    /// Şifre koyar, değiştirir (`Some`) ya da kaldırır (`None`).
    /// Anahtar ve sohbet geçmişi yeni duruma göre yeniden yazılır.
    pub fn set_password(&mut self, password: Option<&str>, history: &History) -> io::Result<()> {
        let key_plain = self.dir.join(KEY_FILE);
        let hist_plain = self.dir.join(HISTORY_FILE);
        let key_enc = self.dir.join(enc_name(KEY_FILE));
        let hist_enc = self.dir.join(enc_name(HISTORY_FILE));
        match password {
            Some(pw) => {
                let key = VaultKey::new(pw);
                write_atomic(&hist_enc, &key.encrypt(&serde_json::to_vec(history)?))?;
                // Anahtar dosyası en son yazılır: hesabın şifreli olup olmadığını o belirler
                write_atomic(&key_enc, &key.encrypt(&self.identity.to_json()))?;
                wipe(&key_plain);
                wipe(&hist_plain);
                self.vault = Some(key);
            }
            None => {
                history.save(&self.dir)?;
                write_atomic(&key_plain, &self.identity.to_json())?;
                let _ = fs::remove_file(&key_enc);
                let _ = fs::remove_file(&hist_enc);
                self.vault = None;
            }
        }
        Ok(())
    }
}

pub fn last_profile(root: &Path) -> Option<String> {
    fs::read_to_string(root.join("last_profile"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn set_last_profile(root: &Path, id: &str) -> io::Result<()> {
    fs::create_dir_all(root)?;
    fs::write(root.join("last_profile"), id)
}

impl History {
    pub fn load(dir: &Path) -> History {
        fs::read(dir.join("history.json"))
            .map(|b| History::from_bytes(&b))
            .unwrap_or_default()
    }

    fn from_bytes(bytes: &[u8]) -> History {
        let mut h: History = serde_json::from_slice(bytes).unwrap_or_default();
        // Uygulama kapanırken yarım kalan gönderimler: kişi çevrimiçi olunca
        // kendiliğinden kaldığı yerden sürer. Gelenler gönderenin devam etmesini bekler.
        for m in &mut h.messages {
            if m.state == MsgState::Pending {
                m.state = if m.outgoing {
                    MsgState::Queued
                } else {
                    MsgState::Failed(
                        "Aktarım yarıda kaldı; gönderen devam ettirince kaldığı yerden sürer"
                            .into(),
                    )
                };
            }
        }
        h
    }

    pub fn save(&self, dir: &Path) -> io::Result<()> {
        write_json(&dir.join("history.json"), self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root() -> PathBuf {
        std::env::temp_dir().join(format!("lanchat-root-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn profiles_create_list_and_lock() {
        let root = temp_root();
        assert!(list_profiles(&root).is_empty());
        let a = create_profile(&root, "Kamil – İş").unwrap();
        let b = create_profile(&root, "Kamil – Kişisel").unwrap();
        assert_ne!(a.id, b.id);
        let list = list_profiles(&root);
        assert_eq!(list.len(), 2);
        assert!(list.iter().all(|p| !p.in_use));

        // İkinci hesabın indirme klasörü ayrı olmalı
        let ca = Config::load(&a.dir).unwrap();
        let cb = Config::load(&b.dir).unwrap();
        assert_ne!(ca.download_dir, cb.download_dir);

        // Aynı hesap iki kez açılamaz, diğeri açılabilir
        let opened = open_profile(&a.dir).unwrap().unlock("").unwrap();
        assert!(matches!(open_profile(&a.dir), Err(ProfileError::InUse)));
        let in_use: Vec<bool> = list_profiles(&root).iter().map(|p| p.in_use).collect();
        assert_eq!(in_use.iter().filter(|x| **x).count(), 1);
        let _b = open_profile(&b.dir).unwrap();
        drop(opened);
        assert!(open_profile(&a.dir).is_ok());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn send_requests_roundtrip() {
        let dir = std::env::temp_dir().join(format!("lanchat-gonder-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let a = vec![PathBuf::from("C:/a b/ç.txt"), PathBuf::from("/tmp/klasör")];
        request_send(&dir, &a).unwrap();
        request_send(&dir, &[PathBuf::from("/x")]).unwrap();
        let mut got = take_send_requests(&dir);
        got.sort();
        let mut want = a.clone();
        want.push(PathBuf::from("/x"));
        want.sort();
        assert_eq!(got, want);
        assert!(
            take_send_requests(&dir).is_empty(),
            "istekler bir kez alınmalı"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn queued_signals_are_coalesced() {
        let mut out = Vec::new();
        let react = |e: &str| Signal::React {
            msg_id: "m".into(),
            emoji: Some(e.into()),
        };
        History::queue_signal(&mut out, "p", react("👍"));
        History::queue_signal(&mut out, "p", react("❤"));
        History::queue_signal(&mut out, "q", react("👍"));
        History::queue_signal(
            &mut out,
            "p",
            Signal::Edit {
                msg_id: "m".into(),
                text: "a".into(),
            },
        );
        History::queue_signal(&mut out, "p", Signal::Nudge);
        // Aynı kişi ve mesaj için yalnızca son tepki kalır; titreşim sıraya girmez
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].signal, react("❤"));
        // Silme, o mesajla ilgili bekleyen her şeyin yerini alır
        History::queue_signal(&mut out, "p", Signal::Delete { msg_id: "m".into() });
        let for_p: Vec<_> = out.iter().filter(|o| o.peer_id == "p").collect();
        assert_eq!(for_p.len(), 1);
        assert_eq!(for_p[0].signal, Signal::Delete { msg_id: "m".into() });
    }

    fn sample_history(text: &str) -> History {
        History {
            peers: vec![],
            messages: vec![ChatMessage::new(
                "m1".into(),
                "p".into(),
                true,
                Body::Text(text.into()),
                MsgState::Done,
            )],
            outbox: vec![],
            groups: vec![],
        }
    }

    fn text_of(h: &History) -> &str {
        match &h.messages[0].body {
            Body::Text(t) => t,
            _ => panic!(),
        }
    }

    /// Klasördeki tüm dosyalarda düz metin arar.
    fn plaintext_on_disk(dir: &Path, needle: &str) -> bool {
        fs::read_dir(dir).unwrap().filter_map(Result::ok).any(|e| {
            fs::read(e.path())
                .is_ok_and(|b| b.windows(needle.len()).any(|w| w == needle.as_bytes()))
        })
    }

    #[test]
    fn password_encrypts_history_and_key_on_disk() {
        let root = temp_root();
        let info = create_profile(&root, "Gizli").unwrap();

        // Şifresiz açılır, geçmiş düz metin
        let mut p = open_profile(&info.dir).unwrap().unlock("").unwrap();
        let public = p.identity.public;
        let h = sample_history("GIZLIMESAJ-42");
        p.save_history(&h).unwrap();
        assert!(plaintext_on_disk(&info.dir, "GIZLIMESAJ-42"));
        assert!(!p.has_password());

        // Şifre konunca diskte ne mesaj ne de gizli anahtar düz metin kalır
        p.set_password(Some("parola1"), &h).unwrap();
        assert!(p.has_password());
        assert!(!plaintext_on_disk(&info.dir, "GIZLIMESAJ-42"));
        assert!(!info.dir.join("key.json").exists());
        assert!(!info.dir.join("history.json").exists());
        assert!(list_profiles(&root)[0].has_password);
        drop(p);

        // Yanlış şifre: açılmaz, hesap kilitli kalır ve yeniden denenebilir
        let locked = open_profile(&info.dir).unwrap();
        assert!(locked.has_password());
        let (locked, err) = locked.unlock("yanlis").unwrap_err();
        assert!(matches!(err, UnlockError::WrongPassword));
        assert!(matches!(open_profile(&info.dir), Err(ProfileError::InUse)));

        // Doğru şifre: aynı anahtar ve geçmiş
        let mut p = locked.unlock("parola1").unwrap();
        assert_eq!(p.identity.public, public);
        assert_eq!(text_of(&p.load_history()), "GIZLIMESAJ-42");
        assert!(p.check_password("parola1"));
        assert!(!p.check_password("parola2"));

        // Şifre değiştir: eskisi artık açmaz
        p.set_password(Some("yeni-parola"), &p.load_history())
            .unwrap();
        drop(p);
        let (locked, _) = open_profile(&info.dir)
            .unwrap()
            .unlock("parola1")
            .unwrap_err();
        let mut p = locked.unlock("yeni-parola").unwrap();
        assert_eq!(text_of(&p.load_history()), "GIZLIMESAJ-42");

        // Şifreyi kaldır: tekrar şifresiz açılır, her şey yerinde
        let h = p.load_history();
        p.set_password(None, &h).unwrap();
        drop(p);
        assert!(!has_password(&info.dir));
        let p = open_profile(&info.dir).unwrap().unlock("").unwrap();
        assert_eq!(p.identity.public, public);
        assert_eq!(text_of(&p.load_history()), "GIZLIMESAJ-42");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn forgotten_password_reset_keeps_settings_but_drops_secrets() {
        let root = temp_root();
        let info = create_profile(&root, "Unutkan").unwrap();
        let mut p = open_profile(&info.dir).unwrap().unlock("").unwrap();
        let old_key = p.identity.public;
        let h = sample_history("ESKI-GECMIS");
        p.set_password(Some("unuttum"), &h).unwrap();
        drop(p);

        let p = open_profile(&info.dir).unwrap().reset_forgotten().unwrap();
        assert_eq!(p.cfg.name, "Unutkan");
        assert!(!p.has_password());
        assert_ne!(p.identity.public, old_key);
        assert!(p.load_history().messages.is_empty());
        assert!(!has_password(&info.dir));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_settings_move_into_first_profile() {
        let root = temp_root();
        let cfg = Config::new("Eski", PathBuf::from("/x"));
        cfg.save(&root).unwrap();
        History::default().save(&root).unwrap();
        let list = list_profiles(&root);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, cfg.id);
        assert_eq!(list[0].name, "Eski");
        assert!(list[0].dir.join("history.json").exists());
        assert!(!root.join("config.json").exists());
        assert_eq!(last_profile(&root).as_deref(), Some(cfg.id.as_str()));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn history_roundtrip_requeues_outgoing_and_fails_incoming() {
        let dir = std::env::temp_dir().join(format!("lanchat-hist-{}", uuid::Uuid::new_v4()));
        let h = History {
            peers: vec![KnownPeer {
                id: "p".into(),
                name: "Ofis".into(),
                addr: Some("192.168.1.5:47821".parse().unwrap()),
                macs: vec!["aa:bb:cc:dd:ee:ff".into()],
            }],
            messages: vec![
                ChatMessage {
                    progress: 5,
                    ..ChatMessage::new(
                        "1".into(),
                        "p".into(),
                        true,
                        Body::File {
                            name: "a.zip".into(),
                            size: 10,
                            path: None,
                            is_dir: false,
                            file_count: 1,
                        },
                        MsgState::Pending,
                    )
                },
                ChatMessage::new(
                    "2".into(),
                    "p".into(),
                    false,
                    Body::Text(String::new()),
                    MsgState::Pending,
                ),
            ],
            outbox: vec![Outgoing {
                peer_id: "p".into(),
                signal: Signal::Delete { msg_id: "9".into() },
            }],
            groups: vec![],
        };
        h.save(&dir).unwrap();
        let loaded = History::load(&dir);
        assert_eq!(loaded.peers[0].name, "Ofis");
        assert_eq!(loaded.peers[0].macs, ["aa:bb:cc:dd:ee:ff"]);
        // Bizim yarım kalan gönderimimiz sıraya döner, gelen yarım aktarım başarısız sayılır
        assert_eq!(loaded.messages[0].state, MsgState::Queued);
        assert!(matches!(loaded.messages[1].state, MsgState::Failed(_)));
        assert_eq!(loaded.messages[0].progress, 0);
        assert_eq!(loaded.outbox, h.outbox);
        fs::remove_dir_all(dir).unwrap();
    }
}
