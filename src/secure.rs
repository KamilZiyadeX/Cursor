//! Uçtan uca şifreli bağlantı.
//!
//! Her hesabın kalıcı bir X25519 anahtar çifti vardır. Her TCP bağlantısı
//! `Noise_XX_25519_ChaChaPoly_BLAKE2s` el sıkışmasıyla başlar: iki taraf
//! birbirinin kalıcı açık anahtarını öğrenir ve sonraki tüm veriler
//! ChaCha20-Poly1305 ile şifrelenip doğrulanır.
//!
//! Hangi anahtara güvenileceği eşleştirmeyle belirlenir: iki ekranda gösterilen
//! 6 haneli kod, iki tarafın anahtarlarından türetilir; araya giren biri
//! (ortadaki adam) farklı kodlara yol açar.

use sha2::{Digest, Sha256};
use std::fs;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::path::Path;
use std::time::Duration;

const NOISE_PARAMS: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
const PROLOGUE: &[u8] = b"LanChat-3";
/// Noise iletisinin en büyük boyutu
const MAX_MSG: usize = 65535;
const TAG_LEN: usize = 16;
const MAX_PLAIN: usize = MAX_MSG - TAG_LEN;

pub type PublicKey = [u8; 32];

/// Bir hesabın kalıcı anahtar çifti.
#[derive(Clone)]
pub struct Identity {
    private: [u8; 32],
    pub public: PublicKey,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Identity({})", to_hex(&self.public))
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct KeyFile {
    private: String,
    public: String,
}

impl Identity {
    pub fn generate() -> Identity {
        let kp = snow::Builder::new(NOISE_PARAMS.parse().expect("geçerli Noise parametreleri"))
            .generate_keypair()
            .expect("anahtar üretilemedi");
        Identity {
            private: kp.private.try_into().expect("32 bayt"),
            public: kp.public.try_into().expect("32 bayt"),
        }
    }

    /// `dir/key.json` dosyasından okur; yoksa yeni anahtar üretip kaydeder.
    pub fn load_or_create(dir: &Path) -> io::Result<Identity> {
        let path = dir.join("key.json");
        if let Ok(bytes) = fs::read(&path) {
            let kf: KeyFile = serde_json::from_slice(&bytes)?;
            let private = from_hex(&kf.private).ok_or_else(|| invalid("bozuk anahtar dosyası"))?;
            let public = from_hex(&kf.public).ok_or_else(|| invalid("bozuk anahtar dosyası"))?;
            return Ok(Identity { private, public });
        }
        let id = Identity::generate();
        let kf = KeyFile {
            private: to_hex(&id.private),
            public: to_hex(&id.public),
        };
        fs::create_dir_all(dir)?;
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(&kf)?)?;
        fs::rename(tmp, &path)?;
        Ok(id)
    }
}

/// İki tarafın açık anahtarlarından türetilen 6 haneli eşleştirme kodu
/// (sıradan bağımsız: iki tarafta da aynı çıkar).
pub fn pairing_code(a: &PublicKey, b: &PublicKey) -> String {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    let mut h = Sha256::new();
    h.update(b"LanChat eslestirme kodu");
    h.update(lo);
    h.update(hi);
    let d = h.finalize();
    let n = u32::from_be_bytes([d[0], d[1], d[2], d[3]]) % 1_000_000;
    format!("{:03} {:03}", n / 1000, n % 1000)
}

/// Şifreli TCP akışı. `Read`/`Write` olarak düz bir akış gibi kullanılır.
pub struct SecureStream {
    tcp: TcpStream,
    noise: snow::TransportState,
    remote: PublicKey,
    rbuf: Vec<u8>,
    rpos: usize,
    rlen: usize,
    cipher: Vec<u8>,
}

impl SecureStream {
    /// Bağlantıyı başlatan taraf.
    pub fn client(tcp: TcpStream, me: &Identity) -> io::Result<SecureStream> {
        let mut hs = snow::Builder::new(NOISE_PARAMS.parse().map_err(noise_err)?)
            .local_private_key(&me.private)
            .map_err(noise_err)?
            .prologue(PROLOGUE)
            .map_err(noise_err)?
            .build_initiator()
            .map_err(noise_err)?;
        let mut tcp = tcp;
        let mut buf = vec![0u8; MAX_MSG];
        let mut payload = vec![0u8; MAX_MSG];
        // -> e
        let n = hs.write_message(&[], &mut buf).map_err(noise_err)?;
        write_msg(&mut tcp, &buf[..n])?;
        // <- e, ee, s, es
        let m = read_msg(&mut tcp)?;
        hs.read_message(&m, &mut payload).map_err(noise_err)?;
        // -> s, se
        let n = hs.write_message(&[], &mut buf).map_err(noise_err)?;
        write_msg(&mut tcp, &buf[..n])?;
        Self::finish(tcp, hs)
    }

    /// Bağlantıyı kabul eden taraf.
    pub fn server(tcp: TcpStream, me: &Identity) -> io::Result<SecureStream> {
        let mut hs = snow::Builder::new(NOISE_PARAMS.parse().map_err(noise_err)?)
            .local_private_key(&me.private)
            .map_err(noise_err)?
            .prologue(PROLOGUE)
            .map_err(noise_err)?
            .build_responder()
            .map_err(noise_err)?;
        let mut tcp = tcp;
        let mut buf = vec![0u8; MAX_MSG];
        let mut payload = vec![0u8; MAX_MSG];
        let m = read_msg(&mut tcp)?;
        hs.read_message(&m, &mut payload).map_err(noise_err)?;
        let n = hs.write_message(&[], &mut buf).map_err(noise_err)?;
        write_msg(&mut tcp, &buf[..n])?;
        let m = read_msg(&mut tcp)?;
        hs.read_message(&m, &mut payload).map_err(noise_err)?;
        Self::finish(tcp, hs)
    }

    fn finish(tcp: TcpStream, hs: snow::HandshakeState) -> io::Result<SecureStream> {
        let remote: PublicKey = hs
            .get_remote_static()
            .and_then(|k| k.try_into().ok())
            .ok_or_else(|| invalid("karşı tarafın anahtarı alınamadı"))?;
        let noise = hs.into_transport_mode().map_err(noise_err)?;
        Ok(SecureStream {
            tcp,
            noise,
            remote,
            rbuf: vec![0u8; MAX_MSG],
            rpos: 0,
            rlen: 0,
            cipher: vec![0u8; MAX_MSG + 2],
        })
    }

    /// El sıkışmada doğrulanan karşı taraf anahtarı.
    pub fn remote_key(&self) -> &PublicKey {
        &self.remote
    }

    pub fn set_read_timeout(&self, t: Option<Duration>) -> io::Result<()> {
        self.tcp.set_read_timeout(t)
    }

    pub fn shutdown(&self) {
        let _ = self.tcp.shutdown(Shutdown::Both);
    }
}

impl Read for SecureStream {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        while self.rpos == self.rlen {
            let msg = match read_msg(&mut self.tcp) {
                Ok(m) => m,
                // Karşı taraf bağlantıyı düzgünce kapattı
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(0),
                Err(e) => return Err(e),
            };
            self.rlen = self
                .noise
                .read_message(&msg, &mut self.rbuf)
                .map_err(noise_err)?;
            self.rpos = 0;
        }
        let n = out.len().min(self.rlen - self.rpos);
        out[..n].copy_from_slice(&self.rbuf[self.rpos..self.rpos + n]);
        self.rpos += n;
        Ok(n)
    }
}

impl Write for SecureStream {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        for chunk in data.chunks(MAX_PLAIN) {
            let n = self
                .noise
                .write_message(chunk, &mut self.cipher[2..])
                .map_err(noise_err)?;
            self.cipher[..2].copy_from_slice(&(n as u16).to_be_bytes());
            self.tcp.write_all(&self.cipher[..n + 2])?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.tcp.flush()
    }
}

fn write_msg(w: &mut impl Write, msg: &[u8]) -> io::Result<()> {
    let mut buf = Vec::with_capacity(msg.len() + 2);
    buf.extend_from_slice(&(msg.len() as u16).to_be_bytes());
    buf.extend_from_slice(msg);
    w.write_all(&buf)
}

fn read_msg(r: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut len = [0u8; 2];
    r.read_exact(&mut len)?;
    let mut msg = vec![0u8; u16::from_be_bytes(len) as usize];
    r.read_exact(&mut msg)?;
    Ok(msg)
}

fn noise_err(e: snow::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("şifreleme hatası: {e}"))
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn from_hex(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    fn pair() -> (SecureStream, SecureStream, Identity, Identity) {
        let a = Identity::generate();
        let b = Identity::generate();
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let bb = b.clone();
        let srv = thread::spawn(move || SecureStream::server(l.accept().unwrap().0, &bb).unwrap());
        let cli = SecureStream::client(TcpStream::connect(addr).unwrap(), &a).unwrap();
        (cli, srv.join().unwrap(), a, b)
    }

    #[test]
    fn handshake_reveals_keys_and_data_roundtrips() {
        let (mut c, mut s, a, b) = pair();
        assert_eq!(c.remote_key(), &b.public);
        assert_eq!(s.remote_key(), &a.public);

        // Büyük veri birden çok Noise iletisine bölünmeli
        let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let d2 = data.clone();
        let w = thread::spawn(move || {
            c.write_all(&d2).unwrap();
            c.write_all(b"son").unwrap();
            c
        });
        let mut got = vec![0u8; data.len() + 3];
        s.read_exact(&mut got).unwrap();
        assert_eq!(&got[..data.len()], &data[..]);
        assert_eq!(&got[data.len()..], b"son");
        drop(w.join().unwrap());
        // Kapanınca okuma 0 döner
        assert_eq!(s.read(&mut [0u8; 4]).unwrap(), 0);
    }

    /// Şifreli aktarım hızı (yalnızca elle: `cargo test --release -- --ignored hiz`)
    #[test]
    #[ignore]
    fn hiz_olcumu() {
        let (mut c, mut s, _, _) = pair();
        const TOTAL: usize = 2 << 30; // 2 GB
        let w = thread::spawn(move || {
            let buf = vec![1u8; 1 << 20];
            for _ in 0..TOTAL / buf.len() {
                c.write_all(&buf).unwrap();
            }
        });
        let start = std::time::Instant::now();
        let mut buf = vec![0u8; 1 << 20];
        let mut got = 0usize;
        while got < TOTAL {
            got += s.read(&mut buf).unwrap();
        }
        w.join().unwrap();
        let secs = start.elapsed().as_secs_f64();
        eprintln!(
            "şifreli aktarım: {:.0} MB/sn",
            TOTAL as f64 / secs / 1048576.0
        );
    }

    #[test]
    fn traffic_is_not_plaintext() {
        // Sunucu tarafında ham TCP'yi dinle; düz metin görünmemeli
        let a = Identity::generate();
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let raw = thread::spawn(move || {
            let (mut t, _) = l.accept().unwrap();
            let mut all = Vec::new();
            let _ = t.read_to_end(&mut all);
            all
        });
        let tcp = TcpStream::connect(addr).unwrap();
        // El sıkışma tamamlanmayacak; yine de ilk iletide anahtar ya da metin olmamalı
        tcp.set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let _ = SecureStream::client(tcp, &a);
        let bytes = raw.join().unwrap();
        assert!(!bytes.windows(32).any(|w| w == a.public));
    }

    #[test]
    fn tampering_is_detected() {
        // İstemci -> [araya giren vekil] -> sunucu. Vekil, el sıkışmadan sonraki
        // ilk şifreli iletide tek bir baytı değiştirir.
        let a = Identity::generate();
        let b = Identity::generate();
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let server_addr = server.local_addr().unwrap();
        let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy_addr = proxy.local_addr().unwrap();

        thread::spawn(move || {
            let (mut from_client, _) = proxy.accept().unwrap();
            let mut to_server = TcpStream::connect(server_addr).unwrap();
            let mut back_client = from_client.try_clone().unwrap();
            let mut back_server = to_server.try_clone().unwrap();
            thread::spawn(move || {
                let _ = io::copy(&mut back_server, &mut back_client);
            });
            let mut buf = [0u8; 4096];
            let mut total = 0usize;
            loop {
                let n = match from_client.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                // El sıkışma (istemci yönü): 34 + 66 = 100 bayt; sonrası şifreli veri
                for (i, byte) in buf[..n].iter_mut().enumerate() {
                    if total + i == 140 {
                        *byte ^= 0x01;
                    }
                }
                total += n;
                if to_server.write_all(&buf[..n]).is_err() {
                    break;
                }
            }
        });

        let bb = b.clone();
        let srv = thread::spawn(move || {
            let mut s = SecureStream::server(server.accept().unwrap().0, &bb).unwrap();
            let mut buf = [0u8; 64];
            s.read_exact(&mut buf).map(|_| buf.to_vec())
        });
        let mut c = SecureStream::client(TcpStream::connect(proxy_addr).unwrap(), &a).unwrap();
        c.write_all(&[7u8; 64]).unwrap();
        let res = srv.join().unwrap();
        let err = res.expect_err("değiştirilen veri kabul edilmemeli");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn pairing_code_is_symmetric_and_key_dependent() {
        let a = Identity::generate();
        let b = Identity::generate();
        let c = Identity::generate();
        let ab = pairing_code(&a.public, &b.public);
        assert_eq!(ab, pairing_code(&b.public, &a.public));
        assert_ne!(ab, pairing_code(&a.public, &c.public));
        assert_eq!(ab.len(), 7);
    }

    #[test]
    fn identity_persists_and_hex_roundtrips() {
        let dir = std::env::temp_dir().join(format!("lanchat-key-{}", uuid::Uuid::new_v4()));
        let a = Identity::load_or_create(&dir).unwrap();
        let b = Identity::load_or_create(&dir).unwrap();
        assert_eq!(a.public, b.public);
        assert_eq!(from_hex(&to_hex(&a.public)), Some(a.public));
        assert_eq!(from_hex("zz"), None);
        fs::remove_dir_all(dir).unwrap();
    }
}
