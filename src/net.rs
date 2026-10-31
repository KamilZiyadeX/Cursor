//! Keşif (UDP yayın) ve mesaj/dosya aktarımı (TCP).
//!
//! Tüm ağ işleri arka plan iş parçacıklarında yürür; sonuçlar arayüze
//! `NetEvent` olarak bir kanal üzerinden iletilir.

use crate::protocol::{self, ACK_ERR, ACK_OK, Beacon, Packet};
use socket2::{Domain, Protocol, Socket, Type};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Write};
use std::net::{
    IpAddr, Ipv4Addr, Shutdown, SocketAddr, SocketAddrV4, TcpListener, TcpStream, UdpSocket,
};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::Duration;

const BEACON_INTERVAL: Duration = Duration::from_secs(3);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const IO_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug)]
pub enum NetEvent {
    PeerSeen {
        id: String,
        name: String,
        addr: SocketAddr,
    },
    IncomingText {
        from: protocol::From,
        addr: SocketAddr,
        msg_id: String,
        text: String,
    },
    IncomingFile {
        from: protocol::From,
        addr: SocketAddr,
        msg_id: String,
        name: String,
        size: u64,
    },
    Progress {
        msg_id: String,
        done: u64,
    },
    Completed {
        msg_id: String,
        saved_path: Option<PathBuf>,
    },
    Failed {
        msg_id: String,
        error: String,
    },
}

pub struct NetOptions {
    pub my_id: String,
    pub name: String,
    pub download_dir: PathBuf,
    pub tcp_port: u16,
    pub discovery_port: u16,
    pub manual_ips: Vec<IpAddr>,
}

/// Arayüzün ağla konuştuğu tutamaç. Klonlanabilir.
#[derive(Clone)]
pub struct Net {
    inner: Arc<Inner>,
}

struct Inner {
    tx: Sender<NetEvent>,
    notify: Box<dyn Fn() + Send + Sync>,
    my_id: String,
    name: RwLock<String>,
    download_dir: RwLock<PathBuf>,
    tcp_port: u16,
    discovery_port: u16,
    unicast_targets: Mutex<HashSet<IpAddr>>,
    discovery_error: Option<String>,
}

impl Net {
    /// Ağ hizmetlerini başlatır. `notify`, yeni olay geldiğinde çağrılır
    /// (arayüzü yeniden çizdirmek için).
    pub fn start(
        opts: NetOptions,
        notify: impl Fn() + Send + Sync + 'static,
    ) -> io::Result<(Net, Receiver<NetEvent>)> {
        let listener = TcpListener::bind(("0.0.0.0", opts.tcp_port))
            .or_else(|_| TcpListener::bind(("0.0.0.0", 0)))?;
        let tcp_port = listener.local_addr()?.port();

        let (udp, discovery_error) = match bind_discovery(opts.discovery_port) {
            Ok(s) => (Some(s), None),
            Err(e) => (None, Some(format!("Otomatik keşif başlatılamadı: {e}"))),
        };

        let (tx, rx) = mpsc::channel();
        let net = Net {
            inner: Arc::new(Inner {
                tx,
                notify: Box::new(notify),
                my_id: opts.my_id,
                name: RwLock::new(opts.name),
                download_dir: RwLock::new(opts.download_dir),
                tcp_port,
                discovery_port: opts.discovery_port,
                unicast_targets: Mutex::new(opts.manual_ips.into_iter().collect()),
                discovery_error,
            }),
        };

        {
            let net = net.clone();
            thread::Builder::new()
                .name("tcp-accept".into())
                .spawn(move || net.accept_loop(listener))?;
        }

        if let Some(udp) = udp {
            let recv = udp.try_clone()?;
            let n1 = net.clone();
            thread::Builder::new()
                .name("discovery-recv".into())
                .spawn(move || n1.discovery_recv_loop(recv))?;
            let n2 = net.clone();
            thread::Builder::new()
                .name("discovery-send".into())
                .spawn(move || n2.discovery_send_loop(udp))?;
        }

        Ok((net, rx))
    }

    pub fn tcp_port(&self) -> u16 {
        self.inner.tcp_port
    }

    pub fn discovery_error(&self) -> Option<&str> {
        self.inner.discovery_error.as_deref()
    }

    pub fn set_name(&self, name: &str) {
        *self.inner.name.write().unwrap() = name.to_string();
    }

    pub fn set_download_dir(&self, dir: PathBuf) {
        *self.inner.download_dir.write().unwrap() = dir;
    }

    /// Bu adrese de doğrudan (unicast) duyuru gönderilir; yayının
    /// ulaşmadığı ağlarda elle eklenen bilgisayarlar için.
    pub fn add_target(&self, ip: IpAddr) {
        self.inner.unicast_targets.lock().unwrap().insert(ip);
    }

    fn emit(&self, ev: NetEvent) {
        let _ = self.inner.tx.send(ev);
        (self.inner.notify)();
    }

    fn me(&self) -> protocol::From {
        protocol::From {
            id: self.inner.my_id.clone(),
            name: self.inner.name.read().unwrap().clone(),
            port: self.inner.tcp_port,
        }
    }

    /* ------------------------------ Keşif ------------------------------ */

    fn discovery_send_loop(&self, sock: UdpSocket) {
        loop {
            let beacon = Beacon::new(
                &self.inner.my_id,
                &self.inner.name.read().unwrap(),
                self.inner.tcp_port,
            );
            if let Ok(data) = serde_json::to_vec(&beacon) {
                let mut targets: HashSet<IpAddr> = broadcast_addresses().into_iter().collect();
                targets.insert(IpAddr::V4(Ipv4Addr::BROADCAST));
                targets.extend(self.inner.unicast_targets.lock().unwrap().iter().copied());
                for ip in targets {
                    let _ = sock.send_to(&data, SocketAddr::new(ip, self.inner.discovery_port));
                }
            }
            thread::sleep(BEACON_INTERVAL);
        }
    }

    fn discovery_recv_loop(&self, sock: UdpSocket) {
        let mut buf = [0u8; 2048];
        loop {
            let Ok((n, src)) = sock.recv_from(&mut buf) else {
                thread::sleep(Duration::from_millis(200));
                continue;
            };
            let Some(b) = Beacon::parse(&buf[..n]) else {
                continue;
            };
            if b.id == self.inner.my_id {
                continue;
            }
            // Karşı tarafa da doğrudan duyuru gönder: yayın tek yönlü
            // ulaşıyorsa bile iki taraf birbirini görsün.
            self.add_target(src.ip());
            self.emit(NetEvent::PeerSeen {
                id: b.id,
                name: b.name,
                addr: SocketAddr::new(src.ip(), b.port),
            });
        }
    }

    /* ------------------------------ Alma ------------------------------ */

    fn accept_loop(&self, listener: TcpListener) {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let net = self.clone();
            let _ = thread::Builder::new()
                .name("tcp-conn".into())
                .spawn(move || net.handle_conn(stream));
        }
    }

    fn handle_conn(&self, mut stream: TcpStream) {
        let Ok(peer) = stream.peer_addr() else { return };
        let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
        let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
        let _ = stream.set_nodelay(true);

        let Ok(packet) = protocol::read_header(&mut stream) else {
            return;
        };
        match packet {
            Packet::Text { from, msg_id, text } => {
                if from.id == self.inner.my_id {
                    return;
                }
                let addr = SocketAddr::new(peer.ip(), from.port);
                self.add_target(peer.ip());
                self.emit(NetEvent::IncomingText {
                    from,
                    addr,
                    msg_id,
                    text,
                });
                let _ = stream.write_all(&[ACK_OK]);
            }
            Packet::File {
                from,
                msg_id,
                name,
                size,
            } => {
                if from.id == self.inner.my_id {
                    return;
                }
                let addr = SocketAddr::new(peer.ip(), from.port);
                self.add_target(peer.ip());
                let name = protocol::sanitize_file_name(&name);
                self.emit(NetEvent::IncomingFile {
                    from,
                    addr,
                    msg_id: msg_id.clone(),
                    name: name.clone(),
                    size,
                });
                match self.receive_file(&mut stream, &msg_id, &name, size) {
                    Ok(path) => {
                        let _ = stream.write_all(&[ACK_OK]);
                        self.emit(NetEvent::Completed {
                            msg_id,
                            saved_path: Some(path),
                        });
                    }
                    Err(e) => {
                        let _ = stream.write_all(&[ACK_ERR]);
                        self.emit(NetEvent::Failed {
                            msg_id,
                            error: e.to_string(),
                        });
                    }
                }
            }
        }
        let _ = stream.shutdown(Shutdown::Both);
    }

    fn receive_file(
        &self,
        stream: &mut TcpStream,
        msg_id: &str,
        name: &str,
        size: u64,
    ) -> io::Result<PathBuf> {
        let dir = self.inner.download_dir.read().unwrap().clone();
        fs::create_dir_all(&dir)?;
        let safe_id: String = msg_id
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .take(40)
            .collect();
        let part = dir.join(format!(".lanchat-{safe_id}.part"));
        let result = (|| {
            let mut file = File::create(&part)?;
            protocol::copy_exact(stream, &mut file, size, |done| {
                self.emit(NetEvent::Progress {
                    msg_id: msg_id.to_string(),
                    done,
                })
            })?;
            file.sync_all()?;
            drop(file);
            let target = unique_path(&dir, name);
            fs::rename(&part, &target)?;
            Ok(target)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&part);
        }
        result
    }

    /* ------------------------------ Gönderme ------------------------------ */

    pub fn send_text(&self, addr: SocketAddr, msg_id: String, text: String) {
        let net = self.clone();
        thread::spawn(move || {
            let packet = Packet::Text {
                from: net.me(),
                msg_id: msg_id.clone(),
                text,
            };
            let res = (|| {
                let mut s = connect(addr)?;
                protocol::write_header(&mut s, &packet)?;
                s.flush()?;
                read_ack(&mut s)
            })();
            net.finish_send(msg_id, res);
        });
    }

    pub fn send_file(&self, addr: SocketAddr, msg_id: String, path: PathBuf) {
        let net = self.clone();
        thread::spawn(move || {
            let res = (|| {
                let file = File::open(&path)?;
                let size = file.metadata()?.len();
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "dosya".into());
                let packet = Packet::File {
                    from: net.me(),
                    msg_id: msg_id.clone(),
                    name,
                    size,
                };
                let mut s = connect(addr)?;
                protocol::write_header(&mut s, &packet)?;
                let mut reader = BufReader::with_capacity(256 * 1024, file);
                protocol::copy_exact(&mut reader, &mut s, size, |done| {
                    net.emit(NetEvent::Progress {
                        msg_id: msg_id.clone(),
                        done,
                    })
                })?;
                read_ack(&mut s)
            })();
            net.finish_send(msg_id, res);
        });
    }

    fn finish_send(&self, msg_id: String, res: io::Result<()>) {
        self.emit(match res {
            Ok(()) => NetEvent::Completed {
                msg_id,
                saved_path: None,
            },
            Err(e) => NetEvent::Failed {
                msg_id,
                error: friendly_error(&e),
            },
        });
    }
}

fn connect(addr: SocketAddr) -> io::Result<TcpStream> {
    let s = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)?;
    s.set_read_timeout(Some(IO_TIMEOUT))?;
    s.set_write_timeout(Some(IO_TIMEOUT))?;
    s.set_nodelay(true)?;
    Ok(s)
}

fn read_ack(s: &mut TcpStream) -> io::Result<()> {
    let mut ack = [0u8; 1];
    s.read_exact(&mut ack)?;
    if ack[0] == ACK_OK {
        Ok(())
    } else {
        Err(io::Error::other("karşı taraf dosyayı kaydedemedi"))
    }
}

fn friendly_error(e: &io::Error) -> String {
    match e.kind() {
        io::ErrorKind::ConnectionRefused => {
            "Bağlantı reddedildi (karşı tarafta LanChat açık mı?)".into()
        }
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => {
            "Zaman aşımı (karşı bilgisayar kapalı ya da güvenlik duvarı engelliyor olabilir)".into()
        }
        io::ErrorKind::NotFound => "Dosya bulunamadı".into(),
        io::ErrorKind::PermissionDenied => "Erişim reddedildi".into(),
        _ => e.to_string(),
    }
}

fn bind_discovery(port: u16) -> io::Result<UdpSocket> {
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    s.set_reuse_address(true)?;
    #[cfg(all(unix, not(any(target_os = "solaris", target_os = "illumos"))))]
    s.set_reuse_port(true)?;
    s.set_broadcast(true)?;
    s.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port).into())?;
    Ok(s.into())
}

/// Her ağ arayüzünün yayın (broadcast) adresi.
fn broadcast_addresses() -> Vec<IpAddr> {
    let mut out = Vec::new();
    if let Ok(ifaces) = if_addrs::get_if_addrs() {
        for i in ifaces {
            if i.is_loopback() {
                continue;
            }
            if let if_addrs::IfAddr::V4(v4) = i.addr {
                if let Some(b) = v4.broadcast {
                    out.push(IpAddr::V4(b));
                } else {
                    let ip = u32::from(v4.ip);
                    let mask = u32::from(v4.netmask);
                    out.push(IpAddr::V4(Ipv4Addr::from(ip | !mask)));
                }
            }
        }
    }
    out
}

/// Bu bilgisayarın yerel ağdaki IPv4 adresleri.
pub fn local_ipv4_addresses() -> Vec<Ipv4Addr> {
    let mut out: Vec<Ipv4Addr> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| !i.is_loopback())
        .filter_map(|i| match i.ip() {
            IpAddr::V4(v4) => Some(v4),
            IpAddr::V6(_) => None,
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// `dir/name` mevcutsa `ad (1).uzantı`, `ad (2).uzantı`… dener.
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (1..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .expect("sonsuz aralık")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lanchat-test-{tag}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn start(id: &str, dir: &Path) -> (Net, Receiver<NetEvent>) {
        Net::start(
            NetOptions {
                my_id: id.into(),
                name: format!("Test {id}"),
                download_dir: dir.to_path_buf(),
                tcp_port: 0,
                // Testlerde keşif portunu çakıştırmamak için rastgele bir port
                discovery_port: 0,
                manual_ips: vec![],
            },
            || {},
        )
        .unwrap()
    }

    fn wait_for(rx: &Receiver<NetEvent>, mut pred: impl FnMut(&NetEvent) -> bool) -> NetEvent {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let ev = rx.recv_timeout(left).expect("olay gelmedi");
            if pred(&ev) {
                return ev;
            }
        }
    }

    #[test]
    fn unique_path_adds_suffix() {
        let d = temp_dir("uniq");
        fs::write(d.join("a.txt"), "x").unwrap();
        fs::write(d.join("a (1).txt"), "x").unwrap();
        assert_eq!(unique_path(&d, "a.txt"), d.join("a (2).txt"));
        assert_eq!(unique_path(&d, "b"), d.join("b"));
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn text_and_file_transfer() {
        let da = temp_dir("a");
        let db = temp_dir("b");
        let (a, rx_a) = start("A", &da);
        let (_b, rx_b) = start("B", &db);
        let addr_b: SocketAddr = ([127, 0, 0, 1], _b.tcp_port()).into();

        a.send_text(addr_b, "t1".into(), "Merhaba ğüşıöç 👋".into());
        match wait_for(&rx_b, |e| matches!(e, NetEvent::IncomingText { .. })) {
            NetEvent::IncomingText {
                from,
                text,
                msg_id,
                addr,
            } => {
                assert_eq!(from.id, "A");
                assert_eq!(text, "Merhaba ğüşıöç 👋");
                assert_eq!(msg_id, "t1");
                assert_eq!(addr.port(), a.tcp_port());
            }
            _ => unreachable!(),
        }
        wait_for(
            &rx_a,
            |e| matches!(e, NetEvent::Completed { msg_id, .. } if msg_id == "t1"),
        );

        // 3 MB'lık dosya, iki kez (ikincisi "(1)" ekiyle kaydedilmeli)
        let src = da.join("büyük dosya.bin");
        let data: Vec<u8> = (0..3_000_000u32).map(|i| (i * 31 % 251) as u8).collect();
        fs::write(&src, &data).unwrap();
        for (i, expected) in ["büyük dosya.bin", "büyük dosya (1).bin"]
            .iter()
            .enumerate()
        {
            let id = format!("f{i}");
            a.send_file(addr_b, id.clone(), src.clone());
            let ev = wait_for(
                &rx_b,
                |e| matches!(e, NetEvent::Completed { msg_id, .. } if *msg_id == id),
            );
            let NetEvent::Completed {
                saved_path: Some(p),
                ..
            } = ev
            else {
                panic!("{ev:?}")
            };
            assert_eq!(p, db.join(expected));
            assert_eq!(fs::read(&p).unwrap(), data);
            wait_for(
                &rx_a,
                |e| matches!(e, NetEvent::Completed { msg_id, .. } if *msg_id == id),
            );
        }
        // geçici .part dosyası kalmamalı
        assert!(
            fs::read_dir(&db).unwrap().all(|e| !e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".part"))
        );

        fs::remove_dir_all(da).unwrap();
        fs::remove_dir_all(db).unwrap();
    }

    #[test]
    fn send_to_closed_port_fails() {
        let d = temp_dir("c");
        let (a, rx) = start("C", &d);
        // Kullanılmayan bir port bul
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        a.send_text(([127, 0, 0, 1], port).into(), "x".into(), "selam".into());
        wait_for(
            &rx,
            |e| matches!(e, NetEvent::Failed { msg_id, .. } if msg_id == "x"),
        );
        fs::remove_dir_all(d).unwrap();
    }
}
