//! Build script: give the setup executable the package's icon.
//!
//! The installer is the first thing a user sees of this package, often as a
//! downloaded file in Explorer. The copy left in the program folder also
//! serves as the icon of the Start-menu shortcuts and of the folder
//! context-menu entry, which start `cmd.exe`.
fn main() {
    println!("cargo:rerun-if-changed=../assets/proton-dicom-package.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        #[cfg(windows)]
        {
            let mut res = winresource::WindowsResource::new();
            res.set_icon("../assets/proton-dicom-package.ico");
            if let Err(e) = res.compile() {
                println!("cargo:warning=could not embed the application icon: {e}");
            }
        }
    }
}
