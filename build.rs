//! Embed the CalLink icon and version information in the Windows executables.
fn main() {
    println!("cargo:rerun-if-changed=packaging/callink.ico");
    #[cfg(windows)]
    {
        if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
            let mut res = winresource::WindowsResource::new();
            res.set_icon("packaging/callink.ico");
            res.set("ProductName", "CalLink");
            res.set("FileDescription", "CalLink — TI-84 Plus family connectivity");
            res.set("LegalCopyright", "MIT License");
            if let Err(e) = res.compile() {
                println!("cargo:warning=could not embed the Windows icon: {e}");
            }
        }
    }
}
