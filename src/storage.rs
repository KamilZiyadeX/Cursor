//! Ayarlar ve sohbet geçmişinin diske kaydedilmesi.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

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
    File {
        name: String,
        size: u64,
        path: Option<PathBuf>,
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
    /// Aktarılan bayt (yalnızca aktarım sürerken anlamlı)
    #[serde(skip)]
    pub progress: u64,
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
                m.state = MsgState::Failed("Aktarım yarıda kaldı".into());
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
                id: "1".into(),
                peer_id: "p".into(),
                outgoing: true,
                ts: 0,
                body: Body::File {
                    name: "a.zip".into(),
                    size: 10,
                    path: None,
                },
                state: MsgState::Pending,
                progress: 5,
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
