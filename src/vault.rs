//! Hesap şifresiyle diskte şifreli saklama.
//!
//! Şifreden anahtar, Argon2id ile türetilir (yavaş ve bellek isteyen bir işlem;
//! şifre tahmin saldırılarını pahalı hale getirir). Dosyalar XChaCha20-Poly1305
//! ile şifrelenir: hem içerik gizlenir hem de değiştirilirse fark edilir.
//!
//! Dosya biçimi: `"LCV1"` + u32 (big-endian) başlık uzunluğu + JSON başlık + şifreli veri.
//! Başlıkta anahtar türetme ayarları, tuz ve nonce bulunur; başlık, şifreli veriye
//! "ek doğrulanmış veri" olarak bağlanır (başlıkla oynanırsa çözme başarısız olur).

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use std::fmt;

const MAGIC: &[u8; 4] = b"LCV1";

/// Argon2id ayarları.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct KdfParams {
    /// Bellek (KiB)
    pub m: u32,
    /// Tur sayısı
    pub t: u32,
    /// Paralellik
    pub p: u32,
}

impl KdfParams {
    /// Masaüstü için: 64 MiB bellek, 3 tur (yaklaşık yarım saniye).
    /// Testlerde hız için çok daha hafif ayarlar kullanılır.
    pub fn recommended() -> KdfParams {
        if cfg!(test) {
            KdfParams {
                m: 1024,
                t: 1,
                p: 1,
            }
        } else {
            KdfParams {
                m: 64 * 1024,
                t: 3,
                p: 1,
            }
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
struct Header {
    v: u32,
    kdf: String,
    #[serde(flatten)]
    params: KdfParams,
    salt: String,
    nonce: String,
}

#[derive(Debug, PartialEq)]
pub enum VaultError {
    /// Şifre yanlış ya da dosya değiştirilmiş
    WrongPassword,
    /// Dosya biçimi tanınmıyor
    Corrupt,
}

impl fmt::Display for VaultError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VaultError::WrongPassword => write!(f, "Şifre yanlış"),
            VaultError::Corrupt => write!(f, "Şifreli dosya bozuk"),
        }
    }
}

impl std::error::Error for VaultError {}

/// Şifreden türetilmiş anahtar (ve onu üreten tuz/ayarlar).
#[derive(Clone)]
pub struct VaultKey {
    key: [u8; 32],
    salt: [u8; 16],
    params: KdfParams,
}

impl fmt::Debug for VaultKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VaultKey(..)")
    }
}

impl VaultKey {
    /// Yeni bir şifre için rastgele tuzla anahtar türetir.
    pub fn new(password: &str) -> VaultKey {
        let mut salt = [0u8; 16];
        getrandom::fill(&mut salt).expect("işletim sistemi rastgele sayı üretemedi");
        Self::derive(password, salt, KdfParams::recommended()).expect("geçerli Argon2 ayarları")
    }

    fn derive(password: &str, salt: [u8; 16], params: KdfParams) -> Result<VaultKey, VaultError> {
        let p =
            Params::new(params.m, params.t, params.p, Some(32)).map_err(|_| VaultError::Corrupt)?;
        let mut key = [0u8; 32];
        Argon2::new(Algorithm::Argon2id, Version::V0x13, p)
            .hash_password_into(password.as_bytes(), &salt, &mut key)
            .map_err(|_| VaultError::Corrupt)?;
        Ok(VaultKey { key, salt, params })
    }

    /// Bu şifre, bu anahtarı üreten şifre mi? (aynı tuz ve ayarlarla yeniden türetir)
    pub fn matches(&self, password: &str) -> bool {
        Self::derive(password, self.salt, self.params)
            .map(|k| constant_time_eq(&k.key, &self.key))
            .unwrap_or(false)
    }

    pub fn encrypt(&self, plain: &[u8]) -> Vec<u8> {
        let mut nonce = [0u8; 24];
        getrandom::fill(&mut nonce).expect("işletim sistemi rastgele sayı üretemedi");
        let header = Header {
            v: 1,
            kdf: "argon2id".into(),
            params: self.params,
            salt: hex(&self.salt),
            nonce: hex(&nonce),
        };
        let header = serde_json::to_vec(&header).expect("başlık JSON'a çevrilebilir");
        let cipher = XChaCha20Poly1305::new_from_slice(&self.key).expect("32 baytlık anahtar");
        let ct = cipher
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: plain,
                    aad: &header,
                },
            )
            .expect("şifreleme başarısız");
        let mut out = Vec::with_capacity(8 + header.len() + ct.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&(header.len() as u32).to_be_bytes());
        out.extend_from_slice(&header);
        out.extend_from_slice(&ct);
        out
    }

    /// Bu anahtarla çözer. Dosya başka bir şifreyle (başka tuzla) yazılmışsa hata verir.
    pub fn decrypt(&self, data: &[u8]) -> Result<Vec<u8>, VaultError> {
        let (header, header_bytes, ct) = parse(data)?;
        if from_hex::<16>(&header.salt) != Some(self.salt) || header.params != self.params {
            return Err(VaultError::WrongPassword);
        }
        open(&self.key, &header, header_bytes, ct)
    }
}

/// Şifreli bir dosyayı şifreyle açar; başarılıysa türetilen anahtarı da döndürür
/// (aynı şifreyle yazılmış diğer dosyaları yeniden türetmeden açmak için).
pub fn unlock(password: &str, data: &[u8]) -> Result<(VaultKey, Vec<u8>), VaultError> {
    let (header, header_bytes, ct) = parse(data)?;
    let salt = from_hex::<16>(&header.salt).ok_or(VaultError::Corrupt)?;
    let key = VaultKey::derive(password, salt, header.params)?;
    let plain = open(&key.key, &header, header_bytes, ct)?;
    Ok((key, plain))
}

fn parse(data: &[u8]) -> Result<(Header, &[u8], &[u8]), VaultError> {
    if data.len() < 8 || &data[..4] != MAGIC {
        return Err(VaultError::Corrupt);
    }
    let len = u32::from_be_bytes(data[4..8].try_into().unwrap()) as usize;
    let header_bytes = data.get(8..8 + len).ok_or(VaultError::Corrupt)?;
    let header: Header = serde_json::from_slice(header_bytes).map_err(|_| VaultError::Corrupt)?;
    if header.v != 1 || header.kdf != "argon2id" {
        return Err(VaultError::Corrupt);
    }
    Ok((header, header_bytes, &data[8 + len..]))
}

fn open(
    key: &[u8; 32],
    header: &Header,
    header_bytes: &[u8],
    ct: &[u8],
) -> Result<Vec<u8>, VaultError> {
    let nonce = from_hex::<24>(&header.nonce).ok_or(VaultError::Corrupt)?;
    let cipher = XChaCha20Poly1305::new_from_slice(key).map_err(|_| VaultError::Corrupt)?;
    cipher
        .decrypt(
            &XNonce::from(nonce),
            Payload {
                msg: ct,
                aad: header_bytes,
            },
        )
        .map_err(|_| VaultError::WrongPassword)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_wrong_password() {
        let key = VaultKey::new("gizli şifre 123");
        let data = key.encrypt("merhaba ğüşıöç".as_bytes());
        assert!(!data.windows(7).any(|w| w == b"merhaba"));

        let (k2, plain) = unlock("gizli şifre 123", &data).unwrap();
        assert_eq!(plain, "merhaba ğüşıöç".as_bytes());
        assert_eq!(k2.decrypt(&data).unwrap(), plain);
        assert!(key.matches("gizli şifre 123"));
        assert!(!key.matches("gizli şifre 124"));

        assert_eq!(
            unlock("yanlış", &data).unwrap_err(),
            VaultError::WrongPassword
        );
        // Başka bir şifreyle türetilmiş anahtar bu dosyayı açamaz
        assert!(VaultKey::new("başka").decrypt(&data).is_err());
    }

    /// Gerçek ayarlarla süre ölçümü (elle: `cargo test --release -- --ignored argon_suresi`)
    #[test]
    #[ignore]
    fn argon_suresi() {
        let params = KdfParams {
            m: 64 * 1024,
            t: 3,
            p: 1,
        };
        let start = std::time::Instant::now();
        VaultKey::derive("deneme", [7u8; 16], params).unwrap();
        eprintln!("Argon2id (64 MiB, 3 tur): {:?}", start.elapsed());
    }

    #[test]
    fn tampering_is_detected() {
        let key = VaultKey::new("pw");
        let mut data = key.encrypt(b"veri");
        let last = data.len() - 1;
        data[last] ^= 1;
        assert_eq!(unlock("pw", &data).unwrap_err(), VaultError::WrongPassword);

        // Başlıktaki ayarlarla oynamak da fark edilir
        let data = key.encrypt(b"veri");
        let text = String::from_utf8_lossy(&data).replace("\"t\":1", "\"t\":2");
        assert!(unlock("pw", text.as_bytes()).is_err());

        assert_eq!(unlock("pw", b"bozuk").unwrap_err(), VaultError::Corrupt);
    }

    #[test]
    fn same_password_gives_different_ciphertext() {
        let key = VaultKey::new("pw");
        assert_ne!(key.encrypt(b"ayni"), key.encrypt(b"ayni"));
    }
}
