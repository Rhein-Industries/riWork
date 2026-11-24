use std::path::PathBuf;

/// This crate's own folder. The NSH CLI (0.5) builds the app's crate from a manifest it writes
/// under `apps/riwork/.nsh/rust/app-crates/riwork-nsh-native`, so `CARGO_MANIFEST_DIR` and the
/// working directory are that generated folder; it leaves the real one in `nsh-source-dir`
/// there (as NSH's own `assets!` reads it). A plain Cargo build has no such file.
fn source_dir() -> PathBuf {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let marker = manifest.join("nsh-source-dir");
    match std::fs::read_to_string(&marker) {
        Ok(dir) => {
            // Only where it exists: Cargo reruns a script whose watched file is missing.
            println!("cargo:rerun-if-changed={}", marker.display());
            PathBuf::from(dir.trim())
        }
        Err(_) => manifest,
    }
}

fn main() {
    let preview = source_dir().join("native/preview.m");
    println!("cargo:rerun-if-changed={}", preview.display());
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    cc::Build::new()
        .file(&preview)
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
