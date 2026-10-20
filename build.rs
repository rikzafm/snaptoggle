fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico")              // resource ID 1  -> "ON"  (also the .exe icon)
            .set_icon_with_id("assets/icon_off.ico", "2"); // resource ID 2 -> "OFF"
        res.set("ProductName", "SnapToggle")
            .set("FileDescription", "Snap Windows one-click toggle");
        res.compile().expect("failed to compile Windows resources");
    }
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=assets/icon_off.ico");
}
