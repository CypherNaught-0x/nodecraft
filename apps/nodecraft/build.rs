//! Windows only: embed the app icon and version info (VERSIONINFO) into `nodecraft.exe`.
//!
//! On every other target this does nothing. A missing resource compiler is a warning, so a
//! cross-compile from macOS or Linux still links, unless `NODECRAFT_REQUIRE_WINRES=1` (set by
//! packaging/windows/package.ps1) turns it into an error.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/app-icon/nodecraft.ico");
    println!("cargo:rerun-if-env-changed=NODECRAFT_REQUIRE_WINRES");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../assets/app-icon/nodecraft.ico")
        .set("ProductName", "NodeCraft")
        .set("FileDescription", "NodeCraft generative media canvas")
        .set("CompanyName", "NodeCraft contributors")
        .set("LegalCopyright", "Copyright (c) the NodeCraft contributors. MIT OR Apache-2.0.")
        .set("OriginalFilename", "nodecraft.exe")
        .set("InternalName", "nodecraft");
    if let Err(e) = res.compile() {
        if std::env::var_os("NODECRAFT_REQUIRE_WINRES").is_some() {
            panic!("embedding Windows resources failed: {e}");
        }
        println!("cargo:warning=nodecraft.exe built without icon/version resources: {e}");
    }
}
