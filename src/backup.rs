//! Hesap yedekleme / geri yükleme ve sohbeti dışa aktarma.
//!
//! Yedek, hesabın klasöründeki dosyaları (ayarlar, anahtar, sohbet geçmişi)
//! içeren bir zip dosyasıdır. Hesaba şifre konduysa anahtar ve geçmiş yedekte de
//! şifreli kalır.

use crate::storage::{self, Body, ChatMessage, Config, ProfileInfo};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// Yedeklenen dosyalar (kilit gibi geçici dosyalar alınmaz)
const FILES: [&str; 5] = [
    "config.json",
    "key.json",
    "key.json.enc",
    "history.json",
    "history.json.enc",
];
const MANIFEST: &str = "lanchat-yedek.json";
pub const EXTENSION: &str = "lanchat-yedek";

/// Hesabın klasörünü `dest` zip dosyasına yedekler.
pub fn backup_profile(dir: &Path, dest: &Path) -> io::Result<()> {
    let tmp = dest.with_extension("yedekleniyor");
    let result = (|| {
        let mut zip = zip::ZipWriter::new(fs::File::create(&tmp)?);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        let manifest = serde_json::json!({
            "v": 1,
            "uygulama": "LanChat",
            "surum": crate::protocol::APP_VERSION,
            "tarih": chrono::Local::now().to_rfc3339(),
        });
        zip.start_file(MANIFEST, opts).map_err(io::Error::other)?;
        zip.write_all(manifest.to_string().as_bytes())?;
        for name in FILES {
            let path = dir.join(name);
            if !path.exists() {
                continue;
            }
            zip.start_file(name, opts).map_err(io::Error::other)?;
            zip.write_all(&fs::read(&path)?)?;
        }
        zip.finish().map_err(io::Error::other)?;
        Ok(())
    })();
    match result {
        Ok(()) => fs::rename(&tmp, dest),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Yedeği yeni bir hesap olarak geri yükler.
pub fn restore_profile(root: &Path, src: &Path) -> Result<ProfileInfo, String> {
    let file = fs::File::open(src).map_err(|e| format!("Yedek açılamadı: {e}"))?;
    let mut zip =
        zip::ZipArchive::new(file).map_err(|_| "Bu bir LanChat yedeği değil".to_string())?;
    if zip.by_name(MANIFEST).is_err() {
        return Err("Bu bir LanChat yedeği değil".into());
    }
    let mut files: Vec<(&str, Vec<u8>)> = Vec::new();
    for name in FILES {
        if let Ok(mut f) = zip.by_name(name) {
            let mut data = Vec::new();
            f.read_to_end(&mut data)
                .map_err(|e| format!("Yedek okunamadı: {e}"))?;
            files.push((name, data));
        }
    }
    let config = files
        .iter()
        .find(|(n, _)| *n == "config.json")
        .ok_or("Yedekte hesap ayarları yok")?;
    let mut cfg: Config =
        serde_json::from_slice(&config.1).map_err(|_| "Yedekteki ayarlar bozuk".to_string())?;
    if !files.iter().any(|(n, _)| n.starts_with("key.json")) {
        return Err("Yedekte hesabın anahtarı yok".into());
    }
    // Kimlik dosya adı olarak kullanılıyor: yalnızca güvenli karakterler
    if cfg.id.is_empty()
        || !cfg
            .id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err("Yedekteki hesap kimliği geçersiz".into());
    }
    let dir = storage::profiles_dir(root).join(&cfg.id);
    if dir.exists() {
        return Err(format!(
            "“{}” hesabı bu bilgisayarda zaten var. Aynı hesabın iki kopyası olamaz.",
            cfg.name
        ));
    }
    fs::create_dir_all(&dir).map_err(|e| format!("Hesap klasörü oluşturulamadı: {e}"))?;
    let mut write_all = || -> io::Result<()> {
        for (name, data) in &files {
            if *name != "config.json" {
                fs::write(dir.join(name), data)?;
            }
        }
        // Yedek başka bilgisayardan geldiyse indirme klasörü burada olmayabilir
        let usable = cfg
            .download_dir
            .parent()
            .is_some_and(|p| p.as_os_str().is_empty() || p.exists());
        if !usable {
            cfg.download_dir = storage::default_download_dir(&cfg.name);
        }
        cfg.save(&dir)
    };
    if let Err(e) = write_all() {
        let _ = fs::remove_dir_all(&dir);
        return Err(format!("Yedek geri yüklenemedi: {e}"));
    }
    Ok(ProfileInfo {
        id: cfg.id.clone(),
        name: cfg.name.clone(),
        has_password: storage::has_password(&dir),
        in_use: false,
        dir,
    })
}

/// Önerilen yedek dosyası adı
pub fn backup_file_name(account: &str) -> String {
    format!(
        "LanChat yedeği - {} - {}.{EXTENSION}",
        crate::protocol::sanitize_file_name(account),
        chrono::Local::now().format("%Y-%m-%d")
    )
}

/* ------------------------------ Dışa aktarma ------------------------------ */

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn local(ts: i64) -> chrono::DateTime<chrono::Local> {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_opt(ts, 0)
        .single()
        .unwrap_or_else(chrono::Local::now)
}

/// Mesajın düz metin hâli
fn body_text(m: &ChatMessage) -> String {
    if m.deleted {
        return "(bu mesaj silindi)".into();
    }
    match &m.body {
        Body::Text(t) => t.clone(),
        Body::File {
            name,
            size,
            is_dir,
            file_count,
            ..
        } => {
            let size = crate::net::format_bytes(*size);
            if *is_dir {
                format!("[Klasör] {name} ({file_count} dosya, {size})")
            } else {
                format!("[Dosya] {name} ({size})")
            }
        }
        Body::Nudge => "📳 Titreşim".into(),
    }
}

fn author<'a>(m: &ChatMessage, me: &'a str, peer: &'a str) -> &'a str {
    if m.outgoing { me } else { peer }
}

/// Sohbeti düz metin olarak verir.
pub fn chat_text(title: &str, me: &str, peer: &str, msgs: &[&ChatMessage]) -> String {
    let mut out = format!(
        "LanChat sohbeti: {title}\nDışa aktarıldı: {}\n",
        chrono::Local::now().format("%d.%m.%Y %H:%M")
    );
    let mut day = String::new();
    for m in msgs {
        let t = local(m.ts);
        let d = t.format("%d.%m.%Y").to_string();
        if d != day {
            out.push_str(&format!("\n--- {d} ---\n"));
            day = d;
        }
        let mut line = format!(
            "[{}] {}: {}",
            t.format("%H:%M"),
            author(m, me, peer),
            body_text(m)
        );
        if m.edited && !m.deleted {
            line.push_str(" (düzenlendi)");
        }
        if !m.reactions.is_empty() {
            let r: Vec<&str> = m.reactions.values().map(String::as_str).collect();
            line.push_str(&format!("  [{}]", r.join(" ")));
        }
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// Sohbeti tek başına açılabilen, biçimli bir HTML sayfası olarak verir.
pub fn chat_html(title: &str, me: &str, peer: &str, msgs: &[&ChatMessage]) -> String {
    let mut body = String::new();
    let mut day = String::new();
    for m in msgs {
        let t = local(m.ts);
        let d = t.format("%d.%m.%Y").to_string();
        if d != day {
            body.push_str(&format!("<div class=\"gun\">{d}</div>\n"));
            day = d;
        }
        if matches!(m.body, Body::Nudge) {
            body.push_str(&format!(
                "<div class=\"titresim\">📳 {} titreşim gönderdi · {}</div>\n",
                escape(author(m, me, peer)),
                t.format("%H:%M")
            ));
            continue;
        }
        let class = if m.outgoing { "giden" } else { "gelen" };
        let mut extra = String::new();
        if let Some(r) = &m.reply_to {
            extra.push_str(&format!(
                "<div class=\"alinti\"><b>{}</b><br>{}</div>",
                escape(&r.author),
                escape(&r.preview)
            ));
        }
        let text = escape(&body_text(m)).replace('\n', "<br>");
        let text = if m.deleted {
            format!("<i>{text}</i>")
        } else {
            text
        };
        let mut meta = t.format("%H:%M").to_string();
        if m.edited && !m.deleted {
            meta.push_str(" · düzenlendi");
        }
        if !m.reactions.is_empty() {
            let r: Vec<&str> = m.reactions.values().map(String::as_str).collect();
            meta.push_str(&format!(" · {}", r.join(" ")));
        }
        body.push_str(&format!(
            "<div class=\"satir {class}\"><div class=\"balon\"><div class=\"kim\">{}</div>{extra}{text}<div class=\"saat\">{}</div></div></div>\n",
            escape(author(m, me, peer)),
            escape(&meta)
        ));
    }
    format!(
        r#"<!doctype html>
<html lang="tr"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>LanChat sohbeti: {title}</title>
<style>
:root {{ --zemin:#f4f4f5; --gelen:#ffffff; --giden:#dbeafe; --yazi:#18181b; --soluk:#71717a; --kirmizi:#dc2626; --kim:#dc2626; }}
@media (prefers-color-scheme: dark) {{
  :root {{ --zemin:#18181b; --gelen:#27272a; --giden:#1e3a8a; --yazi:#f4f4f5; --soluk:#a1a1aa; --kim:#fca5a5; }}
}}
body {{ margin:0; background:var(--zemin); color:var(--yazi); font:15px/1.45 "Segoe UI",system-ui,sans-serif; }}
header {{ padding:16px; border-bottom:1px solid #8884; }}
header h1 {{ margin:0; font-size:18px; }} header p {{ margin:4px 0 0; color:var(--soluk); font-size:13px; }}
main {{ max-width:820px; margin:0 auto; padding:12px 16px 40px; }}
.gun {{ text-align:center; color:var(--soluk); font-size:12px; margin:16px 0 6px; }}
.satir {{ display:flex; margin:4px 0; }} .giden {{ justify-content:flex-end; }}
.balon {{ max-width:72%; padding:6px 10px; border-radius:10px; background:var(--gelen); border:1px solid #8883; overflow-wrap:anywhere; }}
.giden .balon {{ background:var(--giden); }}
.kim {{ font-size:12px; font-weight:600; color:var(--kim); }}
.saat {{ font-size:11px; color:var(--soluk); margin-top:2px; }}
.alinti {{ font-size:12px; border-left:3px solid var(--kirmizi); padding:2px 6px; margin:2px 0 4px; opacity:.8; }}
.titresim {{ text-align:center; color:#ea580c; font-size:12px; font-style:italic; margin:6px 0; }}
</style></head>
<body><header><h1>{title}</h1><p>LanChat sohbeti · {count} mesaj · dışa aktarıldı: {date}</p></header>
<main>
{body}</main></body></html>
"#,
        title = escape(title),
        count = msgs.len(),
        date = chrono::Local::now().format("%d.%m.%Y %H:%M"),
    )
}

/// Önerilen dışa aktarma dosyası adı
pub fn export_file_name(chat: &str) -> String {
    format!(
        "LanChat - {} - {}.html",
        crate::protocol::sanitize_file_name(chat),
        chrono::Local::now().format("%Y-%m-%d")
    )
}

/// Sohbeti uzantıya göre (.txt ya da .html) yazar.
pub fn export_chat(
    path: &Path,
    title: &str,
    me: &str,
    peer: &str,
    msgs: &[&ChatMessage],
) -> io::Result<PathBuf> {
    let is_txt = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("txt"));
    let content = if is_txt {
        chat_text(title, me, peer, msgs)
    } else {
        chat_html(title, me, peer, msgs)
    };
    let path = if is_txt
        || path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("html"))
    {
        path.to_path_buf()
    } else {
        path.with_extension("html")
    };
    fs::write(&path, content)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MsgState;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lanchat-{name}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn backup_and_restore_profile() {
        let a = temp("yedek-a");
        let b = temp("yedek-b");
        let p = storage::create_profile(&a, "Kamil – İş").unwrap();
        fs::write(p.dir.join("history.json"), br#"{"peers":[],"messages":[]}"#).unwrap();
        fs::write(p.dir.join("key.json"), b"{}").unwrap();
        fs::write(p.dir.join("lock"), b"").unwrap();
        let file = a.join(backup_file_name("Kamil – İş"));
        backup_profile(&p.dir, &file).unwrap();

        let restored = restore_profile(&b, &file).unwrap();
        assert_eq!(
            (restored.id.as_str(), restored.name.as_str()),
            (p.id.as_str(), "Kamil – İş")
        );
        assert!(restored.dir.join("key.json").exists());
        assert!(restored.dir.join("history.json").exists());
        assert!(
            !restored.dir.join("lock").exists(),
            "kilit dosyası yedeklenmemeli"
        );
        assert_eq!(storage::list_profiles(&b).len(), 1);

        // Aynı hesap ikinci kez geri yüklenemez
        assert!(
            restore_profile(&b, &file)
                .unwrap_err()
                .contains("zaten var")
        );
        // Yedek olmayan dosya reddedilir
        let junk = a.join("rastgele.zip");
        fs::write(&junk, b"zip degil").unwrap();
        assert!(restore_profile(&b, &junk).is_err());
        fs::remove_dir_all(a).unwrap();
        fs::remove_dir_all(b).unwrap();
    }

    #[test]
    fn export_escapes_html_and_writes_text() {
        let mut m1 = ChatMessage::new(
            "1".into(),
            "p".into(),
            false,
            Body::Text("<script>alert(1)</script> & selam".into()),
            MsgState::Done,
        );
        m1.reactions.insert("ben".into(), "👍".into());
        let mut m2 = ChatMessage::new(
            "2".into(),
            "p".into(),
            true,
            Body::Text("düzeltilmiş".into()),
            MsgState::Done,
        );
        m2.edited = true;
        let msgs = [&m1, &m2];
        let html = chat_html("Kamil – Ofis", "Ben", "Ofis", &msgs);
        assert!(!html.contains("<script>alert"));
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt; &amp; selam"));
        assert!(html.contains("düzenlendi") && html.contains("👍"));

        let txt = chat_text("Kamil – Ofis", "Ben", "Ofis", &msgs);
        assert!(txt.contains("Ofis: <script>alert(1)</script> & selam  [👍]"));
        assert!(txt.contains("Ben: düzeltilmiş (düzenlendi)"));

        let d = temp("disa");
        let written = export_chat(&d.join("sohbet"), "x", "Ben", "Ofis", &msgs).unwrap();
        assert_eq!(written.extension().unwrap(), "html");
        let written = export_chat(&d.join("sohbet.txt"), "x", "Ben", "Ofis", &msgs).unwrap();
        assert!(fs::read_to_string(written).unwrap().contains("selam"));
        fs::remove_dir_all(d).unwrap();
    }
}
