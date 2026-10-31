# LanChat

Aynı internet ağına (aynı modem / Wi‑Fi) bağlı bilgisayarlar arasında **mesaj ve dosya gönderme** masaüstü uygulaması. Rust ile yazıldı.

- Sunucu, hesap ya da internet bağlantısı gerekmez: her şey yerel ağın içinde kalır.
- Aynı ağdaki LanChat'ler birbirini **otomatik bulur**, birkaç saniye içinde kişi listesinde görünür.
- **Mesajlar**: Enter gönderir, Shift+Enter yeni satır ekler. Mesaja sağ tıklayıp kopyalayabilirsiniz.
- **Dosyalar**: 📎 düğmesiyle seçin ya da dosyaları pencereye sürükleyip bırakın. Boyut sınırı yoktur; ilerleme çubuğu gösterilir.
- Gelen dosyalar `İndirilenler/LanChat` klasörüne kaydedilir (Ayarlar'dan değiştirilebilir). Her dosyanın yanında **Aç** ve **Klasörde göster** düğmeleri vardır.
- Okunmamış mesaj sayısı ve görev çubuğunda yanıp sönme ile bildirim.
- Sohbet geçmişi saklanır; uygulama kapatılıp açılınca kaybolmaz.
- Windows, macOS ve Linux'ta çalışır. Tek dosyadır, kurulum gerektirmez.

## Kullanım

1. Programı **her iki bilgisayarda da** açın (ör. evdeki ve işyerindeki).
2. İlk açılışta bilgisayarın adı kullanılır. Sağ üstteki **⚙ Ayarlar**'dan anlaşılır bir ad verin (ör. “Kamil – Ofis”).
3. Birkaç saniye içinde diğer bilgisayar soldaki **Kişiler** listesinde yeşil noktayla görünür. Ona tıklayın ve yazmaya başlayın.

### Windows güvenlik duvarı

İlk açılışta Windows “Bu uygulamaya ağ erişimi izni verilsin mi?” diye sorar. **Özel ağlar** kutusunu işaretleyip **İzin ver** deyin. Bu adım atlanırsa diğer bilgisayar sizi göremez ya da size gönderemez.

### Karşı bilgisayar listede görünmüyorsa

Bazı modemler ve kurumsal ağlar yayın (broadcast) paketlerini engeller. Bu durumda:

1. Karşı bilgisayardaki LanChat'in sol alt köşesinde yazan IP adresine bakın (ör. `192.168.1.42`).
2. Kendi bilgisayarınızda **⚙ Ayarlar → Elle eklenen bilgisayarlar** bölümüne bu adresi yazıp **Ekle**, sonra **Kaydet** deyin.

Bir taraf diğerini eklediğinde iki taraf da birbirini görür.

Bunlara rağmen bağlanılamıyorsa güvenlik duvarında aşağıdaki portlara izin verin:

| Port  | Protokol | Ne için                     |
|-------|----------|-----------------------------|
| 47820 | UDP      | Otomatik keşif              |
| 47821 | TCP      | Mesaj ve dosya aktarımı     |

> Not: Misafir Wi‑Fi'ları ve bazı modemlerdeki “AP izolasyonu” ayarı, aynı ağdaki cihazların birbirine bağlanmasını tamamen engeller. Bu durumda iki bilgisayarın aynı (misafir olmayan) ağa bağlı olduğundan emin olun.

## İndirme

Her gönderimde GitHub Actions Windows, macOS ve Linux sürümlerini otomatik derler:

- **Actions** sekmesi → son başarılı **Derle** çalışması → en alttaki **Artifacts** bölümünden `LanChat-windows.exe`'yi (veya `LanChat-macos` / `LanChat-linux`) indirin.
- `v1.0.0` gibi bir etiket (tag) gönderildiğinde dosyalar ayrıca **Releases** sayfasına eklenir.

Windows SmartScreen “tanınmayan uygulama” uyarısı verirse **Ek bilgi → Yine de çalıştır** deyin (program imzalı olmadığı için).

## Kaynak koddan derleme

[Rust](https://rustup.rs) kurulu olmalı.

```bash
cargo run --release
```

Çalıştırılabilir dosya `target/release/lanchat` (Windows'ta `lanchat.exe`) olarak oluşur.

Testler:

```bash
cargo test
```

## Nasıl çalışır?

- **Keşif**: Her uygulama 3 saniyede bir UDP 47820 portuna kimliğini, adını ve TCP portunu yayınlar. Bir duyuru aldığında karşı tarafa doğrudan (unicast) de duyuru gönderir; böylece yayın tek yönlü ulaşsa bile iki taraf birbirini görür. 10 saniyedir duyuru gelmeyen kişi çevrimdışı görünür.
- **Aktarım**: Her mesaj veya dosya için karşı tarafın TCP portuna (varsayılan 47821) yeni bir bağlantı açılır. Gönderen önce küçük bir JSON başlığı, dosyaysa ardından dosya baytlarını yollar. Alıcı her şeyi diske yazdıktan sonra bir onay baytı döner; gönderen tarafta ✔ işareti bu onayla görünür.
- Gelen dosya önce geçici `.lanchat-….part` adıyla yazılır, tamamlanınca asıl adına taşınır. Aynı adda dosya varsa `ad (1).uzantı` şeklinde kaydedilir. Dosya adındaki klasör kısımları atılır; gönderen, indirme klasörünün dışına dosya yazamaz.

Kaynak kod düzeni:

| Dosya             | İçerik                                      |
|-------------------|---------------------------------------------|
| `src/main.rs`     | Pencere ayarları, uygulama simgesi          |
| `src/app.rs`      | Arayüz (egui)                               |
| `src/net.rs`      | UDP keşif, TCP gönderme/alma                |
| `src/protocol.rs` | Paket biçimi, dosya adı temizleme           |
| `src/storage.rs`  | Ayarlar ve sohbet geçmişinin kaydı          |

### Veri klasörü

Ayarlar (`config.json`) ve sohbet geçmişi (`history.json`) şurada tutulur:

- Windows: `%APPDATA%\LanChat\LanChat\data`
- macOS: `~/Library/Application Support/com.LanChat.LanChat`
- Linux: `~/.local/share/lanchat`

`LANCHAT_DATA_DIR` ortam değişkeniyle başka bir klasör, `LANCHAT_PORT` ile başka bir TCP portu kullanılabilir (aynı bilgisayarda iki kopya çalıştırıp denemek için kullanışlıdır).

## Güvenlik notu

LanChat, güvendiğiniz bir ev / ofis ağı için tasarlandı:

- Trafik **şifrelenmez**; aynı ağdaki biri paketleri dinleyebilir.
- Aynı ağda LanChat çalıştıran herkes size mesaj ve dosya gönderebilir; gelen dosyalar otomatik kaydedilir (ama otomatik açılmaz).

Aynı ağı tanımadığınız kişilerle paylaşıyorsanız (ör. bina genelinde ortak Wi‑Fi) bunu göz önünde bulundurun ve bilmediğiniz kişilerden gelen dosyaları açmayın.
