# LanChat

Aynı internet ağına (aynı modem / Wi‑Fi) bağlı bilgisayarlar arasında **mesaj ve dosya gönderme** masaüstü uygulaması. Rust ile yazıldı.

- Sunucu, hesap ya da internet bağlantısı gerekmez: her şey yerel ağın içinde kalır.
- Aynı ağdaki LanChat'ler birbirini **otomatik bulur**, birkaç saniye içinde kişi listesinde görünür.
- **Mesajlar**: Enter gönderir, Shift+Enter yeni satır ekler. Mesaja sağ tıklayıp kopyalayabilirsiniz.
- **Dosya ve klasörler**: 📎 → *Dosya gönder* / *Klasör gönder* ile seçin ya da pencereye sürükleyip bırakın. **Boyut ve dosya sayısı sınırı yoktur** (1 TB'lık bir klasör de gönderilebilir). Klasörler alt klasörleri, boş klasörleri ve dosyaların değiştirilme tarihleriyle birlikte aynen kopyalanır.
- **Kaldığı yerden devam**: Bağlantı koparsa, bilgisayar kapanırsa ya da **✖ İptal**'e basılırsa yarım kalan kısım silinmez. Gönderen **▶ Kaldığı yerden devam et** dediğinde tamamlanan dosyalar atlanır, yarım kalan dosya kaldığı bayttan sürer.
- Aktarım sırasında **hız** ve **kalan süre** gösterilir. Alıcıda yeterli boş alan yoksa aktarım başlamadan reddedilir.
- Gelen dosyalar `İndirilenler/LanChat` klasörüne kaydedilir (Ayarlar'dan değiştirilebilir). Her dosyanın yanında **Aç** ve **Klasörde göster** düğmeleri vardır.
- **Birden çok hesap**: Aynı Windows oturumunda birden fazla LanChat hesabı (ör. “Kamil – İş” ve “Kamil – Kişisel”) aynı anda ayrı pencerelerde açık olabilir. Her hesabın kendi adı, sohbet geçmişi ve indirme klasörü vardır; aynı bilgisayardaki hesaplar birbirine de mesaj ve dosya gönderebilir.
- Okunmamış mesaj sayısı ve görev çubuğunda yanıp sönme ile bildirim.
- Sohbet geçmişi saklanır; uygulama kapatılıp açılınca kaybolmaz.
- Windows, macOS ve Linux'ta çalışır. Tek dosyadır, kurulum gerektirmez.

## Kullanım

1. Programı **her iki bilgisayarda da** açın (ör. evdeki ve işyerindeki).
2. İlk açılışta bilgisayarın adı kullanılır. Sağ üstteki **⚙ Ayarlar**'dan anlaşılır bir ad verin (ör. “Kamil – Ofis”).
3. Birkaç saniye içinde diğer bilgisayar soldaki **Kişiler** listesinde yeşil noktayla görünür. Ona tıklayın ve yazmaya başlayın.

### Birden çok hesap

- Sağ üstteki **👤 Hesaplar → ➕ Yeni hesap…** ile yeni bir hesap oluşturun; yeni hesap ayrı bir pencerede açılır.
- Var olan bir hesabı açmak için yine **👤 Hesaplar** menüsünden adına tıklayın.
- LanChat simgesine tıklandığında en son kullanılan hesap açılır; o hesap zaten açıksa hangi hesapla devam edileceği sorulur.
- Bir hesap aynı anda yalnızca bir pencerede açık olabilir.
- Belirli bir hesapla açılan bir masaüstü kısayolu için kısayolun **Hedef** alanının sonuna `--hesap "Kamil – İş"` ekleyin (bu adla hesap yoksa oluşturulur).

İlk hesap gelen dosyaları `İndirilenler\LanChat`, sonraki hesaplar `İndirilenler\LanChat (hesap adı)` klasörüne kaydeder; **⚙ Ayarlar**'dan değiştirilebilir. Aynı bilgisayarda birden çok hesap açıkken her biri mesaj için sıradaki portu kullanır (47821, 47822, …).

### Windows güvenlik duvarı

İlk açılışta Windows “Bu uygulamaya ağ erişimi izni verilsin mi?” diye sorar. **Özel ağlar** kutusunu işaretleyip **İzin ver** deyin. Bu adım atlanırsa diğer bilgisayar sizi göremez ya da size gönderemez.

### Çok büyük aktarımlar (yüzlerce GB / TB)

Süreyi belirleyen şey ağ hızıdır. Yaklaşık değerler (1 TB için):

| Bağlantı                         | Gerçek hız       | 1 TB süresi   |
|----------------------------------|------------------|---------------|
| Kablolu, 1 Gbit (iki taraf da)   | ~110 MB/sn       | ~2,5–3 saat   |
| Kablolu, 2,5 Gbit                | ~280 MB/sn       | ~1 saat       |
| Wi‑Fi 6, iyi çekim               | ~50–90 MB/sn     | ~3–6 saat     |
| Wi‑Fi 5 / zayıf çekim            | ~10–40 MB/sn     | ~7–29 saat    |

İpuçları: Mümkünse iki bilgisayarı da kabloyla bağlayın; aktarım sürerken bilgisayarların uyku moduna geçmesini kapatın. Kesinti olursa **▶ Kaldığı yerden devam et** yeterlidir.

Yarım kalan aktarımlar indirme klasöründe `.lanchat-…` adıyla tutulur; 30 gün boyunca devam ettirilmeyenler açılışta otomatik silinir.

### Karşı bilgisayar listede görünmüyorsa

Bazı modemler ve kurumsal ağlar yayın (broadcast) paketlerini engeller. Bu durumda:

1. Karşı bilgisayardaki LanChat'in sol alt köşesinde yazan IP adresine bakın (ör. `192.168.1.42`).
2. Kendi bilgisayarınızda **⚙ Ayarlar → Elle eklenen bilgisayarlar** bölümüne bu adresi yazıp **Ekle**, sonra **Kaydet** deyin.

Bir taraf diğerini eklediğinde iki taraf da birbirini görür.

Bunlara rağmen bağlanılamıyorsa güvenlik duvarında aşağıdaki portlara izin verin:

| Port  | Protokol | Ne için                     |
|-------|----------|-----------------------------|
| 47820 | UDP      | Otomatik keşif              |
| 47821–47830 | TCP | Mesaj ve dosya aktarımı (aynı bilgisayarda birden çok hesap açıksa sıradaki port) |

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
- **Aktarım**: Her mesaj veya dosya/klasör için karşı tarafın TCP portuna (varsayılan 47821) yeni bir bağlantı açılır. Gönderen önce aktarımın adını, toplam boyutunu ve dosya sayısını bildirir; alıcı boş alanı kontrol edip kabul eder. Ardından her klasör ve dosya sırayla gönderilir. Her dosya için alıcı, o dosyanın elinde zaten ne kadarı olduğunu söyler ve gönderen yalnızca kalanını yollar; kaldığı yerden devam etme böyle çalışır. Sonunda alıcı bir onay baytı döner; gönderen tarafta ✔ işareti bu onayla görünür.
- Gelen veri önce geçici `.lanchat-….part` (klasörde `.partdir`) adıyla yazılır, tamamlanınca asıl adına taşınır. Aynı adda dosya/klasör varsa `ad (1)` şeklinde kaydedilir. `..` içeren yollar reddedilir; gönderen, indirme klasörünün dışına dosya yazamaz. Gönderen taraftaki sembolik bağlantılar izlenmez; okunamayan dosyalar atlanır ve kaç tane atlandığı mesajda yazılır.

Kaynak kod düzeni:

| Dosya             | İçerik                                      |
|-------------------|---------------------------------------------|
| `src/main.rs`     | Pencere ayarları, uygulama simgesi          |
| `src/app.rs`      | Arayüz (egui)                               |
| `src/net.rs`      | UDP keşif, TCP gönderme/alma                |
| `src/protocol.rs` | Paket biçimi, dosya adı temizleme           |
| `src/storage.rs`  | Hesaplar, ayarlar ve sohbet geçmişinin kaydı |
| `src/root.rs`     | Açılışta hesap seçimi                        |

### Veri klasörü

Her hesabın ayarları (`config.json`) ve sohbet geçmişi (`history.json`) bu klasörün altında `profiles\<hesap kimliği>\` içinde tutulur. Eski sürümden kalan ayarlar ilk açılışta otomatik olarak ilk hesaba taşınır. Veri klasörü:

- Windows: `%APPDATA%\LanChat\LanChat\data`
- macOS: `~/Library/Application Support/com.LanChat.LanChat`
- Linux: `~/.local/share/lanchat`

`LANCHAT_DATA_DIR` ortam değişkeniyle başka bir klasör, `LANCHAT_PORT` ile başka bir TCP portu kullanılabilir (aynı bilgisayarda iki kopya çalıştırıp denemek için kullanışlıdır).

## Güvenlik notu

LanChat, güvendiğiniz bir ev / ofis ağı için tasarlandı:

- Trafik **şifrelenmez**; aynı ağdaki biri paketleri dinleyebilir.
- Aynı ağda LanChat çalıştıran herkes size mesaj ve dosya gönderebilir; gelen dosyalar otomatik kaydedilir (ama otomatik açılmaz).

Aynı ağı tanımadığınız kişilerle paylaşıyorsanız (ör. bina genelinde ortak Wi‑Fi) bunu göz önünde bulundurun ve bilmediğiniz kişilerden gelen dosyaları açmayın.
