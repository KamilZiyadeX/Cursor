<p align="center"><img src="assets/icon/lanchat-128.png" width="128" height="128" alt="LanChat logosu"></p>

# LanChat

*Bir Kapo uygulaması.*

Aynı internet ağına (aynı modem / Wi‑Fi) bağlı bilgisayarlar arasında **mesaj ve dosya gönderme** masaüstü uygulaması. Rust ile yazıldı.

- Sunucu, hesap ya da internet bağlantısı gerekmez: her şey yerel ağın içinde kalır.
- **Uçtan uca şifreli**: Tüm mesajlar ve dosyalar şifrelenir. Yalnızca eşleştirdiğiniz cihazlar size mesaj ve dosya gönderebilir.
- Aynı ağdaki LanChat'ler birbirini **otomatik bulur**, birkaç saniye içinde kişi listesinde görünür.
- **Mesajlar**: Enter gönderir, Shift+Enter yeni satır ekler. Mesaja sağ tıklayınca **Yanıtla**, **Kopyala**, **Benden sil** ve **Herkesten sil** çıkar. Karşı taraf yazarken “yazıyor…”, mesajınızı okuduğunda mavi ✔✔ görünür.
- **Arama**: Sohbetin sağ üstündeki 🔍 (ya da Ctrl+F) o sohbetteki mesajlarda ve dosya adlarında arar; Türkçe harflerde büyük/küçük ayrımı yapmaz.
- **📢 Herkese**: Kişi listesinin en üstündeki bu sohbetten yazılan mesaj ve gönderilen dosyalar, o an çevrimiçi olan tüm eşleşmiş cihazlara gider.
- **Resim önizleme**: Gönderilen ve gelen resimler (PNG, JPG, GIF, WebP, BMP) sohbette küçük resim olarak görünür.
- **Dosya ve klasörler**: 📎 → *Dosya gönder* / *Klasör gönder* ile seçin ya da pencereye sürükleyip bırakın. **Boyut ve dosya sayısı sınırı yoktur** (1 TB'lık bir klasör de gönderilebilir). Klasörler alt klasörleri, boş klasörleri ve dosyaların değiştirilme tarihleriyle birlikte aynen kopyalanır.
- **Kaldığı yerden devam**: Bağlantı koparsa, bilgisayar kapanırsa ya da **✖ İptal**'e basılırsa yarım kalan kısım silinmez. Gönderen **▶ Kaldığı yerden devam et** dediğinde tamamlanan dosyalar atlanır, yarım kalan dosya kaldığı bayttan sürer.
- Aktarım sırasında **hız** ve **kalan süre** gösterilir. Alıcıda yeterli boş alan yoksa aktarım başlamadan reddedilir.
- Gelen dosyalar `İndirilenler/LanChat` klasörüne kaydedilir (Ayarlar'dan değiştirilebilir). Her dosyanın yanında **Aç** ve **Klasörde göster** düğmeleri vardır.
- **Birden çok hesap**: Aynı Windows oturumunda birden fazla LanChat hesabı (ör. “Kamil – İş” ve “Kamil – Kişisel”) aynı anda ayrı pencerelerde açık olabilir. Her hesabın kendi adı, sohbet geçmişi ve indirme klasörü vardır; aynı bilgisayardaki hesaplar birbirine de mesaj ve dosya gönderebilir.
- **Bildirimler ve arka planda çalışma** (Windows): Yeni mesaj ve dosyalarda Windows bildirimi çıkar. Pencere kapatılınca LanChat saatin yanındaki simgede çalışmaya devam eder; istenirse Windows açılınca kendiliğinden başlar.
- **Hız sınırı**: Büyük aktarımlar ağı tıkamasın diye gönderme hızı sınırlanabilir (isterseniz yalnızca mesai saatlerinde).
- **Ağdan güncelleme**: Eşleştiğiniz bir bilgisayarda daha yeni bir LanChat varsa tek tıkla ondan güncellenir; internetten bir şey indirmeye gerek kalmaz.
- Okunmamış mesaj sayısı, pencere başlığında ve görev çubuğunda yanıp sönme ile gösterilir.
- Sohbet geçmişi saklanır; uygulama kapatılıp açılınca kaybolmaz.
- Windows, macOS ve Linux'ta çalışır. Tek dosyadır, kurulum gerektirmez.

## Kullanım

1. Programı **her iki bilgisayarda da** açın (ör. evdeki ve işyerindeki).
2. İlk açılışta bilgisayarın adı kullanılır. Sağ üstteki **⚙ Ayarlar**'dan anlaşılır bir ad verin (ör. “Kamil – Ofis”).
3. Birkaç saniye içinde diğer bilgisayar soldaki **Kişiler** listesinde yeşil noktayla görünür. Ona tıklayın.
4. İlk seferde **🔑 Eşleştir**'e basın. İki bilgisayarın ekranında da 6 haneli bir kod çıkar. Kodlar aynıysa **iki tarafta da** “Kodlar aynı — Onayla”ya basın. Bundan sonra kişinin yanında 🔒 görünür ve yazışmaya başlayabilirsiniz. Eşleştirme bir kez yapılır.

> Kodlar farklıysa onaylamayın: bu, araya biri girmeye çalışıyor demektir.

### Birden çok hesap

- Sağ üstteki **👤 Hesaplar → ➕ Yeni hesap…** ile yeni bir hesap oluşturun; yeni hesap ayrı bir pencerede açılır.
- Var olan bir hesabı açmak için yine **👤 Hesaplar** menüsünden adına tıklayın.
- LanChat simgesine tıklandığında en son kullanılan hesap açılır; o hesap zaten açıksa hangi hesapla devam edileceği sorulur.
- Bir hesap aynı anda yalnızca bir pencerede açık olabilir.
- Belirli bir hesapla açılan bir masaüstü kısayolu için kısayolun **Hedef** alanının sonuna `--hesap "Kamil – İş"` ekleyin (bu adla hesap yoksa oluşturulur).

İlk hesap gelen dosyaları `İndirilenler\LanChat`, sonraki hesaplar `İndirilenler\LanChat (hesap adı)` klasörüne kaydeder; **⚙ Ayarlar**'dan değiştirilebilir. Aynı bilgisayarda birden çok hesap açıkken her biri mesaj için sıradaki portu kullanır (47821, 47822, …).

### Hesap şifresi (isteğe bağlı)

Her hesaba **⚙ Ayarlar → Hesap şifresi** bölümünden şifre konabilir; zorunlu değildir. Şifre konan hesap:

- açılırken şifre sorar (yanlış şifrede açılmaz),
- sohbet geçmişini ve hesabın şifreleme anahtarını bilgisayarda **şifreli** saklar. Bilgisayara erişen biri dosyaları kopyalasa bile şifre olmadan okuyamaz.

Şifre aynı bölümden değiştirilebilir ya da kaldırılabilir; ikisi için de mevcut şifre istenir. Hesap seçme ekranında şifreli hesapların yanında 🔒 görünür.

> **Şifreyi unutursanız kurtarılamaz.** Giriş ekranındaki **Şifremi unuttum** ile hesap sıfırlanabilir: sohbet geçmişi silinir ve hesabın anahtarı yenilenir (eşleştiğiniz cihazlarla yeniden eşleşmeniz gerekir). Adınız, ayarlarınız ve indirilen dosyalar korunur.
>
> Gelen dosyalar indirme klasörüne normal dosya olarak kaydedilir; hesap şifresi onları şifrelemez.

### Arka planda çalışma, Windows ile başlama, bildirimler (Windows)

- Pencerenin **✕** düğmesine basınca LanChat kapanmaz, saatin yanındaki bildirim alanında (sistem tepsisi) çalışmaya devam eder. Mesaj ve dosyalar gelmeye devam eder. Pencereyi geri açmak için simgeye tıklayın ya da masaüstündeki LanChat'e yeniden çift tıklayın. Tamamen kapatmak için simgeye sağ tıklayıp **Çıkış**'ı seçin.
- Pencere kapalıyken ya da başka bir programdayken mesaj veya dosya gelirse Windows bildirimi çıkar; bildirime tıklayınca o kişinin sohbeti açılır.
- **⚙ Ayarlar → Masaüstü** bölümünden:
  - **Windows açılınca bu hesabı arka planda başlat**: Bilgisayar açılınca LanChat pencere göstermeden, simge olarak başlar. Her hesap için ayrı ayarlanır. (Hesaba şifre konduysa açılışta şifre sorulur.)
  - **Pencere kapatılınca simgede çalışmaya devam et**: Kapatırsanız ✕ programı tamamen kapatır.
  - **Bildirim göster**.

Bir hesap zaten açıkken (simgede gizli olsa bile) LanChat'e tekrar tıklanırsa ikinci bir kopya açılmaz; açık olan pencere öne gelir.

### Gönderme hızı sınırı

**⚙ Ayarlar → Aktarım hızı** bölümünden bu bilgisayarın gönderme hızı MB/sn olarak sınırlanabilir; **Yalnızca şu saatler arasında** seçilirse sınır sadece o saatlerde uygulanır (ör. 09–18 arası 20 MB/sn, akşam sınırsız). Değişiklik, süren aktarımlara da hemen uygulanır.

### Ağ üzerinden güncelleme

Bir bilgisayardaki LanChat güncellendiğinde, onunla eşleşmiş diğer bilgisayarlarda üstte “LanChat'in yeni sürümü … bilgisayarında var” çubuğu çıkar:

1. **Güncelle**'ye basın: yeni program o bilgisayardan şifreli bağlantıyla indirilir ve SHA-256 özetiyle doğrulanır.
2. **Şimdi yeniden başlat**'a basın: program kendini yenisiyle değiştirir ve aynı hesapla yeniden açılır. Sohbet geçmişi ve ayarlar korunur.

Güncelleme yalnızca **eşleşmiş** cihazlardan ve aynı işletim sisteminden alınır. Program, bulunduğu klasöre yazabilmelidir (ör. Masaüstü ya da Belgeler; `Program Files` içine konduysa yönetici izni gerektiğinden güncellenemez). Kullandığınız sürüm, sol alttaki yazıda görünür.

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
| `src/secure.rs`   | Şifreleme (Noise), anahtarlar, eşleştirme kodu |
| `src/vault.rs`    | Hesap şifresiyle diskte şifreli saklama      |
| `src/update.rs`   | Ağdan indirilen yeni sürümü kurma            |
| `src/desktop.rs`  | Sistem tepsisi, Windows ile başlama, bildirimler |
| `src/shell_icon.rs` | Simge değişince Windows Gezgini'ni yenileme |
| `build.rs`        | Windows'ta logoyu .exe dosyasına gömer       |
| `assets/icon/`    | Logo: tüm simge boyutlarında PNG ve `lanchat.ico` |
| `assets/logo/`    | Logonun kaynak SVG dosyaları                 |

### Veri klasörü

Her hesabın ayarları (`config.json`) ve sohbet geçmişi (`history.json`) bu klasörün altında `profiles\<hesap kimliği>\` içinde tutulur. Eski sürümden kalan ayarlar ilk açılışta otomatik olarak ilk hesaba taşınır. Veri klasörü:

- Windows: `%APPDATA%\LanChat\LanChat\data`
- macOS: `~/Library/Application Support/com.LanChat.LanChat`
- Linux: `~/.local/share/lanchat`

`LANCHAT_DATA_DIR` ortam değişkeniyle başka bir klasör, `LANCHAT_PORT` ile başka bir TCP portu kullanılabilir (aynı bilgisayarda iki kopya çalıştırıp denemek için kullanışlıdır).

## Güvenlik

- **Şifreleme**: Her LanChat hesabının kendine ait bir anahtar çifti vardır. Her bağlantı [Noise](https://noiseprotocol.org) protokolüyle (`Noise_XX_25519_ChaChaPoly_BLAKE2s`; WireGuard ve WhatsApp'ın kullandığı çerçeve) başlar. Sonraki tüm mesajlar, dosya adları ve dosya içerikleri ChaCha20-Poly1305 ile şifrelenir ve değiştirilmeye karşı doğrulanır. Ağı dinleyen biri içeriği göremez ve değiştiremez.
- **Eşleştirme**: Hangi cihaza güvenileceğini siz belirlersiniz. Eşleştirme kodu iki tarafın anahtarlarından türetilir; araya giren biri (ortadaki adam saldırısı) farklı kodlara yol açar. Eşleşilmiş cihazlar **⚙ Ayarlar → Eşleşilmiş cihazlar** bölümünde görünür ve oradan kaldırılabilir.
- **Kimlik doğrulama**: Eşleşmemiş cihazlardan gelen mesaj ve dosyalar reddedilir (kullanıcıya bildirilir). Mesaj gönderirken de karşı tarafın anahtarı kontrol edilir; başka biri o kişinin yerine geçemez.
- Aynı bilgisayardaki kendi hesaplarınız birbirine eşleştirmesiz güvenir (aralarındaki trafik yine şifrelidir).
- Hız: Şifreleme saniyede yüzlerce MB işler; kablolu gigabit ağda bile hızı düşürmez.
- Gizli anahtar hesabın veri klasöründe durur. Bu klasör yalnızca sizin Windows kullanıcınıza açıktır. Hesaba şifre konduysa anahtar ve sohbet geçmişi ayrıca şifrelenir: şifreden Argon2id ile (64 MiB bellek, 3 tur) bir anahtar türetilir ve dosyalar XChaCha20-Poly1305 ile şifrelenir (`key.json.enc`, `history.json.enc`).
- Bu sürüm eski (şifresiz) sürümlerle konuşamaz; iki bilgisayarda da yeni sürümü kullanın.

Gelen dosyalar otomatik kaydedilir ama otomatik açılmaz. Eşleştirdiğiniz kişiden bile beklemediğiniz bir dosya gelirse açmadan önce dikkatli olun.

## Logo

Kapo PDF ile aynı aileden: parlak kırmızı zemin, üst üste duran beyaz konuşma balonları, sol üstte kırmızı **LAN** etiketi ve ortada **K-A-P-O** ağı.

- `assets/logo/lanchat.svg`: 64 px ve üstü için ayrıntılı çizim (harfler *Archivo Black* yazı tipiyle).
- `assets/logo/lanchat-medium.svg`: masaüstü boyutları (32, 40 ve 48 px) için; düğümler ve harfler daha büyük.
- `assets/logo/lanchat-16.svg`, `-24`: pencere başlığı ve görev çubuğu gibi en küçük yerler için piksel ızgarasına göre çizilmiş sade sürümler (`lanchat-small-48.svg` uygulama içi üst çubukta kullanılır).
- `assets/icon/`: bu çizimlerden üretilmiş PNG'ler ve hepsini içeren `lanchat.ico`. Pencere simgesi, uygulama içi logo ve Windows'ta `.exe` simgesi buradan alınır.

Windows Gezgini simgeleri önbellekte tuttuğu için bir güncellemede logo değişirse masaüstünde eski simge kalabilir. LanChat açılırken gömülü simgenin değişip değişmediğine bakar; değiştiyse Gezgin'e LanChat dosyasının ve masaüstündeki LanChat kısayollarının simgesinin yenilenmesi gerektiğini bildirir (`src/shell_icon.rs`). Buna rağmen eski simge kalırsa: Görev Yöneticisi → Windows Gezgini → Yeniden başlat.
