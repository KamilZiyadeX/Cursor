//! Ayarlar ve sohbet geçmişinin diske kaydedilmesi.

use crate::secure::{Identity, PublicKey};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum MsgState {
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
}

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct History {
    pub peers: Vec<KnownPeer>,
    pub messages: Vec<ChatMessage>,
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
}

/// Açılmış (kilitlenmiş) bir hesap. Kilit, bu değer yaşadığı sürece tutulur;
/// böylece aynı hesap iki pencerede birden açılamaz.
#[derive(Debug)]
pub struct Profile {
    pub dir: PathBuf,
    pub cfg: Config,
    /// Hesabın şifreleme anahtarı
    pub identity: Identity,
    _lock: fs::File,
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
    })
}

/// Hesabı açar ve kilitler.
pub fn open_profile(dir: &Path) -> Result<Profile, ProfileError> {
    let cfg = Config::load(dir).map_err(ProfileError::Io)?;
    let lock = lock_file(dir).map_err(ProfileError::Io)?;
    match lock.try_lock() {
        Ok(()) => Ok(Profile {
            dir: dir.to_path_buf(),
            cfg,
            identity: Identity::load_or_create(dir).map_err(ProfileError::Io)?,
            _lock: lock,
        }),
        Err(fs::TryLockError::WouldBlock) => Err(ProfileError::InUse),
        Err(fs::TryLockError::Error(e)) => Err(ProfileError::Io(e)),
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
        let mut h: History = fs::read(dir.join("history.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        // Uygulama kapanırken yarım kalan aktarımlar
        for m in &mut h.messages {
            if m.state == MsgState::Pending {
                m.state = MsgState::Failed(if m.outgoing {
                    "Aktarım yarıda kaldı".into()
                } else {
                    "Aktarım yarıda kaldı; gönderen “Devam et” derse kaldığı yerden sürer".into()
                });
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
        let opened = open_profile(&a.dir).unwrap();
        assert!(matches!(open_profile(&a.dir), Err(ProfileError::InUse)));
        let in_use: Vec<bool> = list_profiles(&root).iter().map(|p| p.in_use).collect();
        assert_eq!(in_use.iter().filter(|x| **x).count(), 1);
        let _b = open_profile(&b.dir).unwrap();
        drop(opened);
        assert!(open_profile(&a.dir).is_ok());
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
    fn history_roundtrip_marks_pending_failed() {
        let dir = std::env::temp_dir().join(format!("lanchat-hist-{}", uuid::Uuid::new_v4()));
        let h = History {
            peers: vec![KnownPeer {
                id: "p".into(),
                name: "Ofis".into(),
                addr: Some("192.168.1.5:47821".parse().unwrap()),
            }],
            messages: vec![ChatMessage {
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
            }],
        };
        h.save(&dir).unwrap();
        let loaded = History::load(&dir);
        assert_eq!(loaded.peers[0].name, "Ofis");
        assert!(matches!(loaded.messages[0].state, MsgState::Failed(_)));
        assert_eq!(loaded.messages[0].progress, 0);
        fs::remove_dir_all(dir).unwrap();
    }
}
