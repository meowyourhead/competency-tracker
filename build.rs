fn main() {
    println!("cargo:rerun-if-changed=assets/app.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/app.ico");
        res.set("ProductName", "Competency Tracker");
        res.set("FileDescription", "Competency Tracker");
        res.compile().expect("embed the app icon (assets/app.ico)");
    }
}
