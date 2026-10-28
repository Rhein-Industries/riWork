fn main() {
    println!("cargo:rerun-if-changed=native/preview.m");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    cc::Build::new()
        .file("native/preview.m")
        .flag("-fno-objc-arc")
        .flag("-fblocks")
        .flag("-Wno-deprecated-declarations")
        .compile("riwork_nsh_preview_native");
    for framework in [
        "AppKit",
        "Carbon",
        "CoreGraphics",
        "Foundation",
        "QuartzCore",
    ] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
    println!("cargo:rustc-link-lib=objc");
}
