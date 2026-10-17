//! Windows: embeds the Superba icon (assets/icon/superba.ico) into spb.exe, so
//! Explorer, the taskbar, the Start menu and the MSI show it. Other targets: no-op.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/icon/superba.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut resource = winresource::WindowsResource::new();
    resource.set_icon("../../assets/icon/superba.ico");
    if let Err(error) = resource.compile() {
        // The MSVC build (the release) always has rc.exe; a cross build from Linux
        // needs windres on PATH (llvm-mingw has it). Without it the exe is built
        // without an icon rather than failing a type check.
        if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
            panic!("cannot embed the Windows icon: {error}");
        }
        println!("cargo:warning=Windows icon not embedded: {error}");
    }
}
