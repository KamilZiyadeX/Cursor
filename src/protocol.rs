//! Ağ protokolü.
//!
//! * Keşif: Her uygulama birkaç saniyede bir UDP ile `Beacon` yayınlar.
//! * Aktarım: Her mesaj / aktarım için yeni bir TCP bağlantısı açılır.
//!   Bağlantı düz metin `"LCH3"` ile başlar, ardından Noise el sıkışması gelir
//!   (bkz. `secure.rs`). Bundan sonraki her şey şifrelidir: uzunluk önekli
//!   JSON çerçeveler (u32 big-endian uzunluk + JSON) ve dosya baytları.
//!
//! Metin:
//!   gönderen → `Packet::Text`,  alıcı → 1 bayt onay (`ACK_*`)
//!
//! Eşleştirme:
//!   gönderen → `Packet::PairRequest`,  alıcı (kullanıcı onaylayınca) → `PairReply`
//!
//! Dosya / klasör (kaldığı yerden devam edebilir):
//!   gönderen → `Packet::Transfer`
//!   alıcı    → `Accept` (boş alan yetmiyorsa reddeder)
//!   her öğe için:
//!     gönderen → `Entry::Dir` | `Entry::File`
//!     (yalnızca dosyada) alıcı → u64: zaten elinde olan bayt sayısı (devam noktası)
//!     (yalnızca dosyada) gönderen → kalan baytlar
//!   gönderen → `Entry::End`,  alıcı → 1 bayt onay

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

pub const DISCOVERY_PORT: u16 = 47820;
pub const DEFAULT_TCP_PORT: u16 = 47821;
pub const APP_TAG: &str = "lanchat";
pub const PROTOCOL_VERSION: u32 = 3;

const MAGIC: &[u8; 4] = b"LCH3";
const MAX_FRAME_LEN: u32 = 4 * 1024 * 1024;
const COPY_BUF: usize = 1024 * 1024;

pub const ACK_OK: u8 = 0;
pub const ACK_ERR: u8 = 1;
/// Alıcı göndereni tanımıyor (eşleşme yok)
pub const ACK_UNPAIRED: u8 = 2;

/// Keşif için UDP ile gönderilen duyuru.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Beacon {
    pub app: String,
    pub v: u32,
    pub id: String,
    pub name: String,
    /// Mesajların gönderileceği TCP portu.
    pub port: u16,
}

impl Beacon {
    pub fn new(id: &str, name: &str, port: u16) -> Self {
        Self {
            app: APP_TAG.into(),
            v: PROTOCOL_VERSION,
            id: id.into(),
            name: name.into(),
            port,
        }
    }

    pub fn parse(data: &[u8]) -> Option<Self> {
        let b: Beacon = serde_json::from_slice(data).ok()?;
        (b.app == APP_TAG && !b.id.is_empty()).then_some(b)
    }
}

/// Gönderen bilgisi (her pakette bulunur; böylece alıcı göndereni hemen tanır).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct From {
    pub id: String,
    pub name: String,
    pub port: u16,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Packet {
    Text {
        from: From,
        msg_id: String,
        text: String,
    },
    /// Eşleştirme isteği; alıcıda iki tarafın anahtarlarından türetilen kod gösterilir.
    PairRequest { from: From },
    /// Tek dosya (`is_dir == false`) ya da klasör aktarımı.
    Transfer {
        from: From,
        msg_id: String,
        /// Dosyanın ya da klasörün adı
        name: String,
        is_dir: bool,
        total_size: u64,
        file_count: u64,
    },
}

/// Alıcının `Packet::PairRequest`'e yanıtı.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct PairReply {
    pub accepted: bool,
    pub from: From,
}

/// Alıcının `Packet::Transfer`'a yanıtı.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Accept {
    pub ok: bool,
    pub error: Option<String>,
}

/// Aktarımdaki bir öğe. `path`, klasörün köküne göre `/` ile ayrılmış yoldur.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Entry {
    Dir {
        path: String,
    },
    File {
        path: String,
        size: u64,
        /// Değiştirilme zamanı (Unix saniyesi); alıcı dosyaya aynen uygular.
        mtime: Option<i64>,
    },
    End,
}

pub fn write_frame<T: Serialize>(w: &mut impl Write, value: &T) -> io::Result<()> {
    let json = serde_json::to_vec(value)?;
    let mut buf = Vec::with_capacity(json.len() + 4);
    buf.extend_from_slice(&(json.len() as u32).to_be_bytes());
    buf.extend_from_slice(&json);
    w.write_all(&buf)
}

pub fn read_frame<T: DeserializeOwned>(r: &mut impl Read) -> io::Result<T> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_be_bytes(len);
    if len > MAX_FRAME_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "çerçeve çok büyük",
        ));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf)?;
    Ok(serde_json::from_slice(&buf)?)
}

/// Bağlantının başındaki (şifresiz) protokol işareti.
pub fn write_magic(w: &mut impl Write) -> io::Result<()> {
    w.write_all(MAGIC)
}

pub fn read_magic(r: &mut impl Read) -> io::Result<()> {
    let mut magic = [0u8; 4];
    r.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "geçersiz protokol (karşı taraftaki LanChat sürümü farklı olabilir)",
        ));
    }
    Ok(())
}

pub fn write_u64(w: &mut impl Write, v: u64) -> io::Result<()> {
    w.write_all(&v.to_be_bytes())
}

pub fn read_u64(r: &mut impl Read) -> io::Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_be_bytes(b))
}

pub fn cancelled_error() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "İptal edildi")
}

/// Tam olarak `size` bayt kopyalar. Her parçadan sonra o ana kadar kopyalanan
/// bayt sayısıyla `on_chunk` çağrılır; `cancel` işaretlenirse durur.
pub fn copy_exact(
    r: &mut impl Read,
    w: &mut impl Write,
    size: u64,
    cancel: &AtomicBool,
    mut on_chunk: impl FnMut(u64),
) -> io::Result<()> {
    let mut buf = vec![0u8; COPY_BUF.min(size.max(1) as usize)];
    let mut done = 0u64;
    while done < size {
        if cancel.load(Ordering::Relaxed) {
            return Err(cancelled_error());
        }
        let want = (size - done).min(buf.len() as u64) as usize;
        let n = match r.read(&mut buf[..want]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "bağlantı yarıda kesildi",
                ));
            }
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        w.write_all(&buf[..n])?;
        done += n as u64;
        on_chunk(done);
    }
    w.flush()
}

/// Ağdan gelen dosya adını güvenli bir dosya adına çevirir (klasör kısmı atılır).
pub fn sanitize_file_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let mut out: String = base
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    out = out.trim().trim_end_matches('.').to_string();
    if out.is_empty() || out == "." || out == ".." {
        out = "dosya".into();
    }
    // Windows'un ayrılmış adları
    let stem = out.split('.').next().unwrap_or("").to_ascii_uppercase();
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED.contains(&stem.as_str()) {
        out.insert(0, '_');
    }
    if out.chars().count() > 180 {
        out = out.chars().take(180).collect();
    }
    out
}

/// Aktarımdaki göreli yolu güvenli bir göreli yola çevirir.
/// `..` içeren ya da boş yollar reddedilir; böylece gönderen,
/// hedef klasörün dışına yazamaz.
pub fn safe_relative_path(path: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for part in path.split(['/', '\\']) {
        match part {
            "" | "." => continue,
            ".." => return None,
            p => out.push(sanitize_file_name(p)),
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from() -> From {
        From {
            id: "a".into(),
            name: "Kamil – Ofis".into(),
            port: 1,
        }
    }

    #[test]
    fn header_roundtrip() {
        let p = Packet::Transfer {
            from: from(),
            msg_id: "m1".into(),
            name: "Fotoğraflar".into(),
            is_dir: true,
            total_size: 1 << 40,
            file_count: 12345,
        };
        let mut buf = Vec::new();
        write_magic(&mut buf).unwrap();
        write_frame(&mut buf, &p).unwrap();
        write_frame(&mut buf, &Entry::End).unwrap();
        let mut r = buf.as_slice();
        read_magic(&mut r).unwrap();
        assert_eq!(read_frame::<Packet>(&mut r).unwrap(), p);
        assert_eq!(read_frame::<Entry>(&mut r).unwrap(), Entry::End);
    }

    #[test]
    fn rejects_garbage() {
        assert!(read_magic(&mut &b"HTTP/1.1 200"[..]).is_err());
        assert!(read_magic(&mut &b"LCH2"[..]).is_err());
        assert!(
            Beacon::parse(b"{\"app\":\"other\",\"v\":1,\"id\":\"x\",\"name\":\"n\",\"port\":1}")
                .is_none()
        );
    }

    #[test]
    fn sanitizes_names() {
        assert_eq!(sanitize_file_name("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_file_name("C:\\Users\\x\\a?b.txt"), "a_b.txt");
        assert_eq!(sanitize_file_name(".."), "dosya");
        assert_eq!(sanitize_file_name("con.txt"), "_con.txt");
        assert_eq!(sanitize_file_name("şarkı.mp3"), "şarkı.mp3");
    }

    #[test]
    fn relative_paths_cannot_escape() {
        assert_eq!(safe_relative_path("../x"), None);
        assert_eq!(safe_relative_path("a/../../b"), None);
        assert_eq!(safe_relative_path("a\\..\\b"), None);
        assert_eq!(safe_relative_path(""), None);
        assert_eq!(safe_relative_path("/"), None);
        assert_eq!(
            safe_relative_path("/a//b/./c:d.txt"),
            Some(PathBuf::from("a").join("b").join("c_d.txt"))
        );
    }

    #[test]
    fn copy_exact_detects_eof_and_cancel() {
        let no = AtomicBool::new(false);
        let mut out = Vec::new();
        assert!(copy_exact(&mut &b"abc"[..], &mut out, 5, &no, |_| {}).is_err());
        let mut out = Vec::new();
        copy_exact(&mut &b"abcdef"[..], &mut out, 4, &no, |_| {}).unwrap();
        assert_eq!(out, b"abcd");

        let yes = AtomicBool::new(true);
        let err = copy_exact(&mut &b"abcdef"[..], &mut Vec::new(), 4, &yes, |_| {}).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Interrupted);
    }
}
