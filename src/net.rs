//! Keşif (UDP yayın) ve mesaj/dosya/klasör aktarımı (TCP).
//!
//! Tüm ağ işleri arka plan iş parçacıklarında yürür; sonuçlar arayüze
//! `NetEvent` olarak bir kanal üzerinden iletilir.

use crate::protocol::{self, ACK_ERR, ACK_OK, Accept, Beacon, Entry, Packet};
use socket2::{Domain, Protocol, Socket, Type};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::net::{
    IpAddr, Ipv4Addr, Shutdown, SocketAddr, SocketAddrV4, TcpListener, TcpStream, UdpSocket,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const BEACON_INTERVAL: Duration = Duration::from_secs(3);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const IO_TIMEOUT: Duration = Duration::from_secs(60);
/// Aktarım sonundaki onay için (alıcı büyük dosyaları diske yazarken bekleyebilir)
const FINAL_ACK_TIMEOUT: Duration = Duration::from_secs(600);
const PROGRESS_INTERVAL: Duration = Duration::from_millis(150);
/// Alıcıda boş alan hesabına eklenen pay
const SPACE_MARGIN: u64 = 256 * 1024 * 1024;
/// Bu kadar eski yarım aktarımlar açılışta silinir
const STALE_PARTIAL_AGE: Duration = Duration::from_secs(30 * 24 * 3600);

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
    /// Dosya/klasör gelmeye başladı (ya da yarım kalan aktarım devam ediyor).
    IncomingTransfer {
        from: protocol::From,
        addr: SocketAddr,
        msg_id: String,
        name: String,
        is_dir: bool,
        total_size: u64,
        file_count: u64,
    },
    /// Gönderilecek klasör tarandı; toplam boyut ve dosya sayısı belli oldu.
    Scanned {
        msg_id: String,
        total_size: u64,
        file_count: u64,
    },
    Progress {
        msg_id: String,
        done: u64,
    },
    Completed {
        msg_id: String,
        saved_path: Option<PathBuf>,
        note: Option<String>,
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
    /// Sürmekte olan aktarımların iptal işaretleri
    active: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

/// Gönderilecek bir öğe.
enum LocalEntry {
    Dir {
        rel: String,
    },
    File {
        rel: String,
        abs: PathBuf,
        size: u64,
    },
}

/// İlerleme bildirimlerini seyrekleştirir.
struct Progress<'a> {
    net: &'a Net,
    msg_id: &'a str,
    last: Instant,
}

impl Progress<'_> {
    fn report(&mut self, done: u64, force: bool) {
        if force || self.last.elapsed() >= PROGRESS_INTERVAL {
            self.last = Instant::now();
            self.net.emit(NetEvent::Progress {
                msg_id: self.msg_id.to_string(),
                done,
            });
        }
    }
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
        let download_dir = opts.download_dir.clone();
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
                active: Mutex::new(HashMap::new()),
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

        thread::spawn(move || remove_stale_partials(&download_dir));

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

    /// Sürmekte olan bir aktarımı (gönderme ya da alma) durdurur.
    pub fn cancel(&self, msg_id: &str) {
        if let Some(flag) = self.inner.active.lock().unwrap().get(msg_id) {
            flag.store(true, Ordering::Relaxed);
        }
    }

    fn register(&self, msg_id: &str) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(false));
        self.inner
            .active
            .lock()
            .unwrap()
            .insert(msg_id.to_string(), flag.clone());
        flag
    }

    fn unregister(&self, msg_id: &str, flag: &Arc<AtomicBool>) {
        let mut active = self.inner.active.lock().unwrap();
        if active.get(msg_id).is_some_and(|f| Arc::ptr_eq(f, flag)) {
            active.remove(msg_id);
        }
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
            Packet::Transfer {
                from,
                msg_id,
                name,
                is_dir,
                total_size,
                file_count,
            } => {
                if from.id == self.inner.my_id {
                    return;
                }
                let addr = SocketAddr::new(peer.ip(), from.port);
                self.add_target(peer.ip());
                let name = protocol::sanitize_file_name(&name);
                self.emit(NetEvent::IncomingTransfer {
                    from,
                    addr,
                    msg_id: msg_id.clone(),
                    name: name.clone(),
                    is_dir,
                    total_size,
                    file_count,
                });
                let cancel = self.register(&msg_id);
                let res =
                    self.receive_transfer(&mut stream, &msg_id, &name, is_dir, total_size, &cancel);
                self.unregister(&msg_id, &cancel);
                match res {
                    Ok(path) => {
                        let _ = stream.write_all(&[ACK_OK]);
                        self.emit(NetEvent::Completed {
                            msg_id,
                            saved_path: Some(path),
                            note: None,
                        });
                    }
                    Err(e) => {
                        let _ = stream.write_all(&[ACK_ERR]);
                        self.emit(NetEvent::Failed {
                            msg_id,
                            error: friendly_error(&e, Side::Receiver),
                        });
                    }
                }
            }
        }
        let _ = stream.shutdown(Shutdown::Both);
    }

    fn receive_transfer(
        &self,
        stream: &mut TcpStream,
        msg_id: &str,
        name: &str,
        is_dir: bool,
        total_size: u64,
        cancel: &AtomicBool,
    ) -> io::Result<PathBuf> {
        let dir = self.inner.download_dir.read().unwrap().clone();
        fs::create_dir_all(&dir)?;
        let part = partial_path(&dir, msg_id, is_dir);

        // Boş alan kontrolü (yarım kalmış kısım zaten diskte)
        let needed = total_size.saturating_sub(disk_usage(&part));
        if let Ok(free) = fs4::available_space(&dir)
            && free < needed.saturating_add(SPACE_MARGIN)
        {
            let msg = format!(
                "Alıcı bilgisayarda yeterli boş alan yok ({} gerekli, {} boş)",
                format_bytes(needed),
                format_bytes(free)
            );
            protocol::write_frame(
                stream,
                &Accept {
                    ok: false,
                    error: Some(msg.clone()),
                },
            )?;
            return Err(io::Error::other(msg));
        }
        protocol::write_frame(
            stream,
            &Accept {
                ok: true,
                error: None,
            },
        )?;
        if is_dir {
            fs::create_dir_all(&part)?;
        }

        let mut progress = Progress {
            net: self,
            msg_id,
            last: Instant::now(),
        };
        let mut done = 0u64;
        let mut files_seen = 0u64;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(protocol::cancelled_error());
            }
            match protocol::read_frame::<Entry>(stream)? {
                Entry::Dir { path } => {
                    if !is_dir {
                        return Err(invalid("beklenmeyen klasör"));
                    }
                    let rel = protocol::safe_relative_path(&path)
                        .ok_or_else(|| invalid("geçersiz yol"))?;
                    fs::create_dir_all(part.join(rel))?;
                }
                Entry::File { path, size, mtime } => {
                    files_seen += 1;
                    let target = if is_dir {
                        let rel = protocol::safe_relative_path(&path)
                            .ok_or_else(|| invalid("geçersiz yol"))?;
                        part.join(rel)
                    } else if files_seen == 1 {
                        part.clone()
                    } else {
                        return Err(invalid("tek dosyalık aktarımda birden çok dosya"));
                    };
                    if let Some(parent) = target.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    let offset = resume_offset(&target, size, mtime);
                    let mut file = OpenOptions::new()
                        .create(true)
                        .write(true)
                        .truncate(false)
                        .open(&target)?;
                    if offset == 0 {
                        file.set_len(0)?;
                    } else {
                        file.seek(SeekFrom::Start(offset))?;
                    }
                    protocol::write_u64(stream, offset)?;
                    done += offset;
                    progress.report(done, true);
                    let base = done;
                    let mut w = BufWriter::with_capacity(1024 * 1024, &mut file);
                    protocol::copy_exact(stream, &mut w, size - offset, cancel, |n| {
                        progress.report(base + n, false)
                    })?;
                    drop(w);
                    done += size - offset;
                    if let Some(t) = mtime.and_then(unix_to_system_time) {
                        let _ = file.set_modified(t);
                    }
                }
                Entry::End => break,
            }
        }
        progress.report(done, true);

        let target = unique_path(&dir, name);
        fs::rename(&part, &target)?;
        Ok(target)
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
            net.emit(match res {
                Ok(()) => NetEvent::Completed {
                    msg_id,
                    saved_path: None,
                    note: None,
                },
                Err(e) => NetEvent::Failed {
                    msg_id,
                    error: friendly_error(&e, Side::Sender),
                },
            });
        });
    }

    /// Bir dosyayı ya da klasörü (içindekilerle birlikte) gönderir.
    /// Aynı `msg_id` ile yeniden çağrılırsa alıcı kaldığı yerden devam eder.
    pub fn send_path(&self, addr: SocketAddr, msg_id: String, path: PathBuf) {
        let net = self.clone();
        thread::spawn(move || {
            let cancel = net.register(&msg_id);
            let res = net.do_send(addr, &msg_id, &path, &cancel);
            net.unregister(&msg_id, &cancel);
            net.emit(match res {
                Ok(skipped) => NetEvent::Completed {
                    msg_id,
                    saved_path: None,
                    note: (skipped > 0).then(|| format!("{skipped} öğe okunamadığı için atlandı")),
                },
                Err(e) => NetEvent::Failed {
                    msg_id,
                    error: friendly_error(&e, Side::Sender),
                },
            });
        });
    }

    /// Başarılıysa atlanan (okunamayan) öğe sayısını döndürür.
    fn do_send(
        &self,
        addr: SocketAddr,
        msg_id: &str,
        path: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<u64> {
        let meta = fs::metadata(path)?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "dosya".into());
        let is_dir = meta.is_dir();

        let (entries, mut skipped) = if is_dir {
            scan_dir(path, cancel)?
        } else {
            (
                vec![LocalEntry::File {
                    rel: name.clone(),
                    abs: path.to_path_buf(),
                    size: meta.len(),
                }],
                0,
            )
        };
        let (total_size, file_count) = entries.iter().fold((0u64, 0u64), |(s, c), e| match e {
            LocalEntry::File { size, .. } => (s + size, c + 1),
            LocalEntry::Dir { .. } => (s, c),
        });
        self.emit(NetEvent::Scanned {
            msg_id: msg_id.to_string(),
            total_size,
            file_count,
        });

        let mut s = connect(addr)?;
        protocol::write_header(
            &mut s,
            &Packet::Transfer {
                from: self.me(),
                msg_id: msg_id.to_string(),
                name,
                is_dir,
                total_size,
                file_count,
            },
        )?;
        let accept: Accept = protocol::read_frame(&mut s)?;
        if !accept.ok {
            return Err(io::Error::other(
                accept
                    .error
                    .unwrap_or_else(|| "Karşı taraf aktarımı reddetti".into()),
            ));
        }

        let mut progress = Progress {
            net: self,
            msg_id,
            last: Instant::now(),
        };
        let mut done = 0u64;
        for entry in entries {
            if cancel.load(Ordering::Relaxed) {
                return Err(protocol::cancelled_error());
            }
            match entry {
                LocalEntry::Dir { rel } => {
                    protocol::write_frame(&mut s, &Entry::Dir { path: rel })?;
                }
                LocalEntry::File { rel, abs, size } => {
                    let Ok(file) = File::open(&abs) else {
                        skipped += 1;
                        done += size;
                        continue;
                    };
                    let fmeta = file.metadata()?;
                    let size_now = fmeta.len();
                    let mtime = fmeta.modified().ok().and_then(system_time_to_unix);
                    protocol::write_frame(
                        &mut s,
                        &Entry::File {
                            path: rel,
                            size: size_now,
                            mtime,
                        },
                    )?;
                    let offset = protocol::read_u64(&mut s)?.min(size_now);
                    let mut reader = BufReader::with_capacity(1024 * 1024, file);
                    reader.seek(SeekFrom::Start(offset))?;
                    done += offset;
                    progress.report(done, true);
                    let base = done;
                    protocol::copy_exact(&mut reader, &mut s, size_now - offset, cancel, |n| {
                        progress.report(base + n, false)
                    })?;
                    done += size_now - offset;
                    // Dosya taranırken ve gönderilirken boyutu değiştiyse
                    // toplamın tutarlı kalması için farkı düzelt
                    done = done.saturating_sub(size_now).saturating_add(size);
                }
            }
        }
        protocol::write_frame(&mut s, &Entry::End)?;
        progress.report(done, true);
        s.set_read_timeout(Some(FINAL_ACK_TIMEOUT))?;
        read_ack(&mut s)?;
        Ok(skipped)
    }
}

/// Klasörü (sembolik bağlantıları izlemeden) tarar.
/// Okunamayan alt klasörler atlanır ve sayılır.
fn scan_dir(root: &Path, cancel: &AtomicBool) -> io::Result<(Vec<LocalEntry>, u64)> {
    let mut out = Vec::new();
    let mut skipped = 0u64;
    let mut stack: Vec<(PathBuf, String)> = vec![(root.to_path_buf(), String::new())];
    let mut first = true;
    while let Some((dir, rel)) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            return Err(protocol::cancelled_error());
        }
        let read = match fs::read_dir(&dir) {
            Ok(r) => r,
            // Kök klasör okunamıyorsa bu bir hatadır; alt klasörler atlanır
            Err(e) if first => return Err(e),
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        first = false;
        let mut children: Vec<_> = read.filter_map(Result::ok).collect();
        children.sort_by_key(|e| e.file_name());
        // Yığın ters sırada işlediği için alt klasörleri ters ekle
        let mut subdirs = Vec::new();
        for child in children {
            let Ok(ft) = child.file_type() else {
                skipped += 1;
                continue;
            };
            let child_name = child.file_name().to_string_lossy().into_owned();
            let child_rel = if rel.is_empty() {
                child_name
            } else {
                format!("{rel}/{child_name}")
            };
            if ft.is_symlink() {
                continue;
            } else if ft.is_dir() {
                out.push(LocalEntry::Dir {
                    rel: child_rel.clone(),
                });
                subdirs.push((child.path(), child_rel));
            } else if ft.is_file() {
                match child.metadata() {
                    Ok(m) => out.push(LocalEntry::File {
                        rel: child_rel,
                        abs: child.path(),
                        size: m.len(),
                    }),
                    Err(_) => skipped += 1,
                }
            }
        }
        stack.extend(subdirs.into_iter().rev());
    }
    Ok((out, skipped))
}

/// Alıcıda yarım kalmış dosya varsa ne kadarının yeniden kullanılabileceği.
fn resume_offset(target: &Path, size: u64, mtime: Option<i64>) -> u64 {
    let Ok(meta) = fs::metadata(target) else {
        return 0;
    };
    let have = meta.len();
    if have < size {
        // Yarım dosya: kaldığı yerden devam
        return have;
    }
    if have == size {
        // Tamamlanmış dosya: değiştirilme zamanı da tutuyorsa atla
        let same_time = match (mtime, meta.modified().ok().and_then(system_time_to_unix)) {
            (Some(a), Some(b)) => (a - b).abs() <= 2,
            (None, _) => true,
            _ => false,
        };
        if same_time {
            return size;
        }
    }
    0
}

fn partial_path(dir: &Path, msg_id: &str, is_dir: bool) -> PathBuf {
    let safe_id: String = msg_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(40)
        .collect();
    let ext = if is_dir { "partdir" } else { "part" };
    dir.join(format!(".lanchat-{safe_id}.{ext}"))
}

/// Bir dosyanın ya da klasörün diskte kapladığı toplam bayt.
fn disk_usage(path: &Path) -> u64 {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    fs::read_dir(path)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .map(|e| disk_usage(&e.path()))
                .sum()
        })
        .unwrap_or(0)
}

/// Uzun süredir devam ettirilmeyen yarım aktarımları siler.
fn remove_stale_partials(dir: &Path) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.filter_map(Result::ok) {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.starts_with(".lanchat-") {
            continue;
        }
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > STALE_PARTIAL_AGE);
        if old {
            let p = e.path();
            let _ = if p.is_dir() {
                fs::remove_dir_all(p)
            } else {
                fs::remove_file(p)
            };
        }
    }
}

fn system_time_to_unix(t: SystemTime) -> Option<i64> {
    t.duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64)
}

fn unix_to_system_time(secs: i64) -> Option<SystemTime> {
    u64::try_from(secs)
        .ok()
        .map(|s| UNIX_EPOCH + Duration::from_secs(s))
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
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
        Err(io::Error::other("Karşı taraf aktarımı tamamlayamadı"))
    }
}

#[derive(Clone, Copy)]
enum Side {
    Sender,
    Receiver,
}

fn friendly_error(e: &io::Error, side: Side) -> String {
    use io::ErrorKind::*;
    match (e.kind(), side) {
        (Interrupted, _) => "İptal edildi".into(),
        (ConnectionRefused, _) => "Bağlantı reddedildi (karşı tarafta LanChat açık mı?)".into(),
        (TimedOut | WouldBlock, _) => {
            "Zaman aşımı (karşı bilgisayar kapalı ya da güvenlik duvarı engelliyor olabilir)".into()
        }
        (ConnectionReset | ConnectionAborted | BrokenPipe | UnexpectedEof, Side::Sender) => {
            "Bağlantı koptu ya da karşı taraf aktarımı durdurdu".into()
        }
        (ConnectionReset | ConnectionAborted | BrokenPipe | UnexpectedEof, Side::Receiver) => {
            "Bağlantı koptu ya da gönderen aktarımı durdurdu".into()
        }
        (NotFound, Side::Sender) => "Dosya ya da klasör bulunamadı".into(),
        (PermissionDenied, _) => "Erişim reddedildi".into(),
        (StorageFull, _) => "Disk dolu".into(),
        _ => e.to_string(),
    }
}

pub fn format_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64 / 1024.0;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    let s = if v < 10.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.0}")
    };
    format!("{} {}", s.replace('.', ","), UNITS[i])
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
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let ev = rx.recv_timeout(left).expect("olay gelmedi");
            if pred(&ev) {
                return ev;
            }
        }
    }

    fn completed(rx: &Receiver<NetEvent>, id: &str) -> NetEvent {
        wait_for(rx, |e| match e {
            NetEvent::Completed { msg_id, .. } => msg_id == id,
            NetEvent::Failed { msg_id, error } if msg_id == id => panic!("başarısız: {error}"),
            _ => false,
        })
    }

    fn pattern(len: usize, seed: u32) -> Vec<u8> {
        (0..len as u32)
            .map(|i| (i.wrapping_mul(31).wrapping_add(seed) % 251) as u8)
            .collect()
    }

    /// Klasör ağacını (göreli yol -> içerik; klasörler için None) okur.
    fn tree(root: &Path) -> Vec<(String, Option<Vec<u8>>)> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(&d).unwrap() {
                let p = e.unwrap().path();
                let rel = p
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                if p.is_dir() {
                    out.push((rel, None));
                    stack.push(p);
                } else {
                    out.push((rel, Some(fs::read(&p).unwrap())));
                }
            }
        }
        out.sort();
        out
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
        let (b, rx_b) = start("B", &db);
        let addr_b: SocketAddr = ([127, 0, 0, 1], b.tcp_port()).into();

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
        completed(&rx_a, "t1");

        // 3 MB'lık dosya, iki kez (ikincisi "(1)" ekiyle kaydedilmeli)
        let src = da.join("büyük dosya.bin");
        let data = pattern(3_000_000, 7);
        fs::write(&src, &data).unwrap();
        for (i, expected) in ["büyük dosya.bin", "büyük dosya (1).bin"]
            .iter()
            .enumerate()
        {
            let id = format!("f{i}");
            a.send_path(addr_b, id.clone(), src.clone());
            let NetEvent::Completed {
                saved_path: Some(p),
                ..
            } = completed(&rx_b, &id)
            else {
                panic!()
            };
            assert_eq!(p, db.join(expected));
            assert_eq!(fs::read(&p).unwrap(), data);
            completed(&rx_a, &id);
        }
        // geçici dosya kalmamalı
        assert!(fs::read_dir(&db).unwrap().all(|e| {
            !e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".lanchat-")
        }));

        fs::remove_dir_all(da).unwrap();
        fs::remove_dir_all(db).unwrap();
    }

    #[test]
    fn folder_transfer_preserves_tree_and_times() {
        let da = temp_dir("fa");
        let db = temp_dir("fb");
        let (a, rx_a) = start("A", &da);
        let (b, rx_b) = start("B", &db);
        let addr_b: SocketAddr = ([127, 0, 0, 1], b.tcp_port()).into();

        let src = da.join("Fotoğraflar 2026");
        fs::create_dir_all(src.join("Tatil/Gün 1")).unwrap();
        fs::create_dir_all(src.join("boş klasör")).unwrap();
        fs::write(src.join("not.txt"), "merhaba").unwrap();
        fs::write(src.join("boş.dat"), "").unwrap();
        fs::write(src.join("Tatil/deniz.jpg"), pattern(2_500_000, 1)).unwrap();
        fs::write(src.join("Tatil/Gün 1/kumsal.jpg"), pattern(700_000, 2)).unwrap();
        let old = UNIX_EPOCH + Duration::from_secs(1_600_000_000);
        File::options()
            .write(true)
            .open(src.join("not.txt"))
            .unwrap()
            .set_modified(old)
            .unwrap();

        a.send_path(addr_b, "dir1".into(), src.clone());
        let scanned = wait_for(&rx_a, |e| matches!(e, NetEvent::Scanned { .. }));
        let NetEvent::Scanned {
            total_size,
            file_count,
            ..
        } = scanned
        else {
            unreachable!()
        };
        assert_eq!(file_count, 4);
        assert_eq!(total_size, 7 + 2_500_000 + 700_000);

        let NetEvent::Completed {
            saved_path: Some(p),
            ..
        } = completed(&rx_b, "dir1")
        else {
            panic!()
        };
        completed(&rx_a, "dir1");
        assert_eq!(p, db.join("Fotoğraflar 2026"));
        assert_eq!(tree(&p), tree(&src));
        let mtime = fs::metadata(p.join("not.txt")).unwrap().modified().unwrap();
        assert_eq!(mtime, old);

        fs::remove_dir_all(da).unwrap();
        fs::remove_dir_all(db).unwrap();
    }

    #[test]
    fn interrupted_folder_resumes_where_it_left_off() {
        let da = temp_dir("ra");
        let db = temp_dir("rb");
        let (a, rx_a) = start("A", &da);
        let (b, rx_b) = start("B", &db);
        let addr_b: SocketAddr = ([127, 0, 0, 1], b.tcp_port()).into();

        let src = da.join("Yedek");
        fs::create_dir_all(src.join("alt")).unwrap();
        let first = pattern(1_000_000, 3);
        let second = pattern(4_000_000, 4);
        fs::write(src.join("a.bin"), &first).unwrap();
        fs::write(src.join("alt/b.bin"), &second).unwrap();

        // Önceki denemeden kalmış gibi: a.bin tamam, b.bin yarıda
        let part = partial_path(&db, "res1", true);
        fs::create_dir_all(part.join("alt")).unwrap();
        fs::copy(src.join("a.bin"), part.join("a.bin")).unwrap();
        let src_mtime = fs::metadata(src.join("a.bin")).unwrap().modified().unwrap();
        File::options()
            .write(true)
            .open(part.join("a.bin"))
            .unwrap()
            .set_modified(src_mtime)
            .unwrap();
        fs::write(part.join("alt/b.bin"), &second[..1_500_000]).unwrap();

        a.send_path(addr_b, "res1".into(), src.clone());
        // İlk ilerleme bildirimi baştan değil, eldeki kısımdan başlamalı
        let NetEvent::Progress { done, .. } =
            wait_for(&rx_a, |e| matches!(e, NetEvent::Progress { .. }))
        else {
            unreachable!()
        };
        assert!(done >= 1_000_000, "devam etmedi, ilk ilerleme: {done}");

        let NetEvent::Completed {
            saved_path: Some(p),
            ..
        } = completed(&rx_b, "res1")
        else {
            panic!()
        };
        completed(&rx_a, "res1");
        assert_eq!(fs::read(p.join("a.bin")).unwrap(), first);
        assert_eq!(fs::read(p.join("alt/b.bin")).unwrap(), second);
        assert!(!part.exists());

        fs::remove_dir_all(da).unwrap();
        fs::remove_dir_all(db).unwrap();
    }

    #[test]
    fn cancel_then_resume_large_folder() {
        let da = temp_dir("ca");
        let db = temp_dir("cb");
        let (a, rx_a) = start("A", &da);
        let (b, rx_b) = start("B", &db);
        let addr_b: SocketAddr = ([127, 0, 0, 1], b.tcp_port()).into();

        let src = da.join("Arşiv");
        fs::create_dir_all(src.join("alt")).unwrap();
        let files = [
            ("1.bin", pattern(64 << 20, 11)),
            ("alt/2.bin", pattern(64 << 20, 12)),
            ("alt/3.bin", pattern(64 << 20, 13)),
        ];
        for (name, data) in &files {
            fs::write(src.join(name), data).unwrap();
        }
        let total = 3 * (64u64 << 20);

        // Başlat ve ilk ilerleme bildiriminde iptal et
        a.send_path(addr_b, "big".into(), src.clone());
        wait_for(&rx_a, |e| matches!(e, NetEvent::Progress { .. }));
        a.cancel("big");
        let NetEvent::Failed { error, .. } = wait_for(
            &rx_a,
            |e| matches!(e, NetEvent::Failed { msg_id, .. } if msg_id == "big"),
        ) else {
            unreachable!()
        };
        assert_eq!(error, "İptal edildi");
        wait_for(
            &rx_b,
            |e| matches!(e, NetEvent::Failed { msg_id, .. } if msg_id == "big"),
        );
        let part = partial_path(&db, "big", true);
        let have = disk_usage(&part);
        assert!(part.exists() && have < total, "yarım kalmalıydı: {have}");

        // Devam et: kalan kısım gönderilir, sonuç birebir aynı olmalı
        a.send_path(addr_b, "big".into(), src.clone());
        let NetEvent::Completed {
            saved_path: Some(p),
            ..
        } = completed(&rx_b, "big")
        else {
            panic!()
        };
        completed(&rx_a, "big");
        for (name, data) in &files {
            assert!(fs::read(p.join(name)).unwrap() == *data, "{name} farklı");
        }
        assert!(!part.exists());

        fs::remove_dir_all(da).unwrap();
        fs::remove_dir_all(db).unwrap();
    }

    #[test]
    fn changed_complete_file_is_resent() {
        let d = temp_dir("rs");
        let f = d.join("x");
        fs::write(&f, b"abcd").unwrap();
        let t = fs::metadata(&f).unwrap().modified().unwrap();
        let secs = system_time_to_unix(t).unwrap();
        assert_eq!(resume_offset(&f, 4, Some(secs)), 4);
        assert_eq!(resume_offset(&f, 4, Some(secs - 1000)), 0);
        assert_eq!(resume_offset(&f, 10, Some(secs)), 4);
        assert_eq!(resume_offset(&f, 2, Some(secs)), 0);
        assert_eq!(resume_offset(&d.join("yok"), 2, None), 0);
        fs::remove_dir_all(d).unwrap();
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

    #[test]
    fn sizes() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1536), "1,5 KB");
        assert_eq!(format_bytes(3 * 1024 * 1024), "3,0 MB");
        assert_eq!(format_bytes(1 << 40), "1,0 TB");
    }
}
