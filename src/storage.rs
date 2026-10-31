//! Ayarlar ve sohbet geçmişinin diske kaydedilmesi.

use serde::{Deserialize, Serialize};
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

pub fn data_dir() -> PathBuf {
    if let Ok(d) = std::env::var("LANCHAT_DATA_DIR") {
        return PathBuf::from(d);
    }
    directories::ProjectDirs::from("com", "LanChat", "LanChat")
        .map(|p| p.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("lanchat-data"))
}

fn default_download_dir() -> PathBuf {
    let base = directories::UserDirs::new()
        .and_then(|u| {
            u.download_dir()
                .map(Path::to_path_buf)
                .or_else(|| Some(u.home_dir().to_path_buf()))
        })
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("LanChat")
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
    pub fn load_or_create(dir: &Path) -> Config {
        let path = dir.join("config.json");
        if let Ok(cfg) = fs::read(&path)
            .map_err(|_| ())
            .and_then(|b| serde_json::from_slice::<Config>(&b).map_err(|_| ()))
        {
            return cfg;
        }
        let cfg = Config {
            id: uuid::Uuid::new_v4().simple().to_string(),
            name: default_name(),
            download_dir: default_download_dir(),
            manual_ips: Vec::new(),
        };
        let _ = cfg.save(dir);
        cfg
    }

    pub fn save(&self, dir: &Path) -> io::Result<()> {
        write_json(&dir.join("config.json"), self)
    }
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
