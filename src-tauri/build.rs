fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        cc::Build::new()
            .file("native/export.m")
            .flag("-fobjc-arc")
            .compile("export");
        println!("cargo:rerun-if-changed=native/export.m");
        println!("cargo:rustc-link-lib=framework=AppKit");
    }
    // `tauri::generate_context!` embeds the icon at macro expansion time, so the
    // crate must be recompiled whenever the icon changes. Without these lines a
    // `tauri dev` run keeps showing the icon baked in by the previous build.
    println!("cargo:rerun-if-changed=icons/icon.icns");
    println!("cargo:rerun-if-changed=icons/icon.png");
    tauri_build::build()
}
