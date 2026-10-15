//! Windows: embed the app icon (shown by Explorer, the taskbar and Alt+Tab) and version info in
//! the exe. Nothing to do on other systems.

fn main() {
    println!("cargo:rerun-if-changed=assets/app.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/app.ico")
        .set("ProductName", "Monitor Input Switcher")
        .set("FileDescription", "Monitor Input Switcher")
        .set("OriginalFilename", "monitor-input-switcher.exe")
        .set(
            "LegalCopyright",
            "Copyright (C) 2026 Allan ELKAIM. GPL-3.0-or-later.",
        );
    if let Err(e) = res.compile() {
        // Happens when cross-checking from Linux without a resource compiler; a real Windows
        // build has one (MSVC's rc.exe).
        println!("cargo:warning=Could not embed the Windows icon: {e}");
    }
}
