use std::fs;
use std::path::Path;

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap() == "windows" {
        let mut res = winres::WindowsResource::new();
        res.set_icon("icon.ico");
        // UAC Manifest: Request admin rights on manual start
        // Task Scheduler with /RL HIGHEST bypasses UAC prompt on autostart
        res.set_manifest(r#"
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="requireAdministrator" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
</assembly>
"#);
        res.compile().unwrap();
    }

    // Copy README to release folder
    let out_dir = std::env::var("OUT_DIR").unwrap_or_default();
    if out_dir.contains("release") {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        let src = Path::new(&manifest_dir).join("README.md");
        let target_dir = Path::new(&manifest_dir).join("target").join("release");
        let dst = target_dir.join("README.md");

        if src.exists() {
            let _ = fs::create_dir_all(&target_dir);
            let _ = fs::copy(&src, &dst);
        }
    }
}
