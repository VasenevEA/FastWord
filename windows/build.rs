// Embeds the app icon (extracted from the macOS AppIcon.icns) as a Windows
// resource: it becomes the exe's Explorer icon and is loaded at runtime for
// the tray (resource id 1, see app_icon() in main.rs).

fn main() {
    println!("cargo:rerun-if-changed=assets/fastword.ico");
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/fastword.ico");
        if let Err(e) = res.compile() {
            // Missing rc.exe shouldn't make the build unusable — the app
            // falls back to the generic application icon at runtime.
            println!("cargo:warning=icon resource not embedded: {e}");
        }
    }
}
