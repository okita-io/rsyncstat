//! Embed the app icon in the Windows executable. Other targets need nothing.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("ProductName", "RSyncStat");
        res.set("FileDescription", "RSyncStat");
        if let Err(err) = res.compile() {
            panic!("could not embed the Windows icon: {err}");
        }
    }
}
