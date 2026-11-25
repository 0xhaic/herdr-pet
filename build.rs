//! Embeds the Windows exe icon and application manifest.
//!
//! Resource compilation needs the Windows SDK, so it only runs for native
//! Windows builds (CI and local Windows builds). Cross-compiling from macOS
//! for the clippy pre-flight skips it; the shipped exe always comes from the
//! Windows CI runner.

fn main() {
    println!("cargo:rerun-if-changed=packaging/windows/herdr-pet.ico");
    println!("cargo:rerun-if-changed=packaging/windows/herdr-pet.manifest");

    let host = std::env::var("HOST").unwrap_or_default();
    let target = std::env::var("TARGET").unwrap_or_default();
    if !host.contains("windows") || !target.contains("windows") {
        return;
    }

    if let Err(error) = winresource::WindowsResource::new()
        .set_icon("packaging/windows/herdr-pet.ico")
        .set_manifest_file("packaging/windows/herdr-pet.manifest")
        .compile()
    {
        panic!("cannot embed the Windows icon and manifest: {error}");
    }
}
