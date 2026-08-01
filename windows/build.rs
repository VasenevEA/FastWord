// Embeds the app icon (extracted from the macOS AppIcon.icns) as a Windows
// resource: it becomes the exe's Explorer icon and is loaded at runtime for
// the tray (resource id 1, see app_icon() in main.rs).

fn main() {
    println!("cargo:rerun-if-changed=assets/fastword.ico");
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/fastword.ico");
        // comctl32 v6 for themed (non-Win95) controls in the main window.
        res.set_manifest(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls"
        version="6.0.0.0" processorArchitecture="*"
        publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true</dpiAware>
    </windowsSettings>
  </application>
</assembly>"#,
        );
        if let Err(e) = res.compile() {
            // Missing rc.exe shouldn't make the build unusable — the app
            // falls back to the generic application icon at runtime.
            println!("cargo:warning=icon resource not embedded: {e}");
        }
    }
}
