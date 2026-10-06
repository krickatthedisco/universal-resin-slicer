//! Embed a Windows version resource and application manifest.
//!
//! The MinGW cross build otherwise has an empty resource directory and
//! declares the Windows XP subsystem. Defender's machine-learning scan
//! treats that shape as Trojan:Win32/Wacatac.B!ml.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        return;
    }

    let version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    let mut numbers = version.split('.');
    let major = numbers.next().unwrap_or("0");
    let minor = numbers.next().unwrap_or("0");
    let patch = numbers.next().unwrap_or("0");
    let ver_comma = format!("{major},{minor},{patch},0");
    let ver_dot = format!("{major}.{minor}.{patch}.0");

    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let manifest_path = out.join("amber.manifest");
    let rc_path = out.join("amber.rc");
    let obj_path = out.join("amber-res.o");

    let manifest = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="com.krickatthedisco.amber" version="{ver_dot}" processorArchitecture="amd64"/>
  <description>Amber resin slicer</description>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}}"/>
    </application>
  </compatibility>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
    </windowsSettings>
  </application>
</assembly>
"#
    );
    fs::write(&manifest_path, manifest).expect("write manifest");

    let manifest_rc = manifest_path.display().to_string().replace('\\', "/");
    let rc = format!(
        r#"
1 VERSIONINFO
FILEVERSION {ver_comma}
PRODUCTVERSION {ver_comma}
FILEFLAGSMASK 0x3f
FILEFLAGS 0
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "Tyler Krick\0"
      VALUE "FileDescription", "Amber resin slicer\0"
      VALUE "FileVersion", "{ver_dot}\0"
      VALUE "InternalName", "amber\0"
      VALUE "LegalCopyright", "MIT License\0"
      VALUE "OriginalFilename", "amber.exe\0"
      VALUE "ProductName", "Amber\0"
      VALUE "ProductVersion", "{ver_dot}\0"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END

1 24 "{manifest_rc}"
"#
    );
    fs::write(&rc_path, rc).expect("write rc");

    let windres = env::var("WINDRES").unwrap_or_else(|_| "x86_64-w64-mingw32-windres".into());
    let status = Command::new(&windres)
        .arg("-O")
        .arg("coff")
        .arg("-i")
        .arg(&rc_path)
        .arg("-o")
        .arg(&obj_path)
        .status()
        .unwrap_or_else(|err| panic!("run {windres}: {err}"));
    if !status.success() {
        panic!("{windres} failed to embed the Windows version resource");
    }
    println!("cargo:rustc-link-arg={}", obj_path.display());
}
