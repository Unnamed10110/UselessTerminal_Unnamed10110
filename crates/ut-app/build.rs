//! Embeds the exe icon and version info (shortcuts, taskbar pins and Explorer read them from the file).

fn main() {
    println!("cargo:rerun-if-changed=assets/app.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/app.ico")
        .set("ProductName", "Useless Terminal")
        .set("FileDescription", "Useless Terminal")
        .set("OriginalFilename", "UselessTerminal.exe")
        .set("InternalName", "UselessTerminal");
    if let Err(e) = res.compile() {
        // A missing resource compiler must not stop development builds.
        println!("cargo:warning=exe resources not embedded: {e}");
    }
}
