//! Ağ protokolü.
//!
//! * Keşif: Her uygulama birkaç saniyede bir UDP ile `Beacon` yayınlar.
//! * Aktarım: Her mesaj / dosya için yeni bir TCP bağlantısı açılır:
//!   `"LCH1"` + u32 (big-endian) başlık uzunluğu + JSON başlık [+ dosya baytları].
//!   Alıcı her şeyi aldıktan sonra tek bir onay baytı döndürür.

use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

pub const DISCOVERY_PORT: u16 = 47820;
pub const DEFAULT_TCP_PORT: u16 = 47821;
pub const APP_TAG: &str = "lanchat";
pub const PROTOCOL_VERSION: u32 = 1;

const MAGIC: &[u8; 4] = b"LCH1";
const MAX_HEADER_LEN: u32 = 4 * 1024 * 1024;

pub const ACK_OK: u8 = 0;
pub const ACK_ERR: u8 = 1;

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
    File {
        from: From,
        msg_id: String,
        name: String,
        size: u64,
    },
}

pub fn write_header(w: &mut impl Write, p: &Packet) -> io::Result<()> {
    let json = serde_json::to_vec(p)?;
    w.write_all(MAGIC)?;
    w.write_all(&(json.len() as u32).to_be_bytes())?;
    w.write_all(&json)
}

pub fn read_header(r: &mut impl Read) -> io::Result<Packet> {
    let mut magic = [0u8; 4];
    r.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "geçersiz protokol",
        ));
    }
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_be_bytes(len);
    if len > MAX_HEADER_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "başlık çok büyük",
        ));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf)?;
    Ok(serde_json::from_slice(&buf)?)
}

/// Tam olarak `size` bayt kopyalar ve en fazla ~10 kez/sn ilerleme bildirir.
pub fn copy_exact(
    r: &mut impl Read,
    w: &mut impl Write,
    size: u64,
    mut on_progress: impl FnMut(u64),
) -> io::Result<()> {
    let mut buf = vec![0u8; 256 * 1024];
    let mut done = 0u64;
    let mut last = Instant::now();
    while done < size {
        let want = (size - done).min(buf.len() as u64) as usize;
        let n = r.read(&mut buf[..want])?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "bağlantı yarıda kesildi",
            ));
        }
        w.write_all(&buf[..n])?;
        done += n as u64;
        if last.elapsed() >= Duration::from_millis(100) {
            on_progress(done);
            last = Instant::now();
        }
    }
    w.flush()?;
    on_progress(done);
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrip() {
        let p = Packet::File {
            from: From {
                id: "a".into(),
                name: "Kamil – Ofis".into(),
                port: 1,
            },
            msg_id: "m1".into(),
            name: "rapor ğüş.pdf".into(),
            size: 42,
        };
        let mut buf = Vec::new();
        write_header(&mut buf, &p).unwrap();
        assert_eq!(read_header(&mut buf.as_slice()).unwrap(), p);
    }

    #[test]
    fn rejects_garbage() {
        assert!(read_header(&mut &b"HTTP/1.1 200"[..]).is_err());
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
    fn copy_exact_detects_eof() {
        let mut out = Vec::new();
        assert!(copy_exact(&mut &b"abc"[..], &mut out, 5, |_| {}).is_err());
        let mut out = Vec::new();
        copy_exact(&mut &b"abcdef"[..], &mut out, 4, |_| {}).unwrap();
        assert_eq!(out, b"abcd");
    }
}
