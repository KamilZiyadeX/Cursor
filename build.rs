//! Windows'ta uygulama simgesini ve sürüm bilgisini .exe dosyasına gömer.
//! Böylece masaüstünde, kısayollarda ve Dosya Gezgini'nde LanChat logosu görünür.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon/lanchat.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon/lanchat.ico");
        res.set("ProductName", "LanChat");
        res.set("FileDescription", "LanChat");
        if let Err(e) = res.compile() {
            panic!("Windows kaynak dosyası derlenemedi: {e}");
        }
    }
}
