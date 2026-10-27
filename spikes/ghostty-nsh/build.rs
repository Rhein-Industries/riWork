//! Throwaway build: links a prebuilt Ghostty archive, never GPUI.
use std::{env, path::PathBuf};

fn main() {
    assert_eq!(env::var("CARGO_CFG_TARGET_OS").unwrap(), "macos");
    for variable in ["GHOSTTY_LIB_DIR", "GHOSTTY_INCLUDE_DIR"] {
        println!("cargo:rerun-if-env-changed={variable}");
    }
    let user_home = PathBuf::from(env::var_os("HOME").expect("HOME"));
    let library = env::var_os("GHOSTTY_LIB_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            user_home
                .join("Library/Caches/gpui-libghostty/native/b1b8e8c04734504b2bf3bf7553ee247f/lib")
        });
    let include = env::var_os("GHOSTTY_INCLUDE_DIR").map(PathBuf::from).unwrap_or_else(|| {
        user_home.join(".cargo/registry/src/index.crates.io-1949cf8c6b5b557f/gpui-libghostty-0.3.1/vendor/ghostty/include")
    });
    assert!(
        library.join("libghostty-internal.a").is_file(),
        "Set GHOSTTY_LIB_DIR to a directory containing the prebuilt libghostty-internal.a"
    );
    assert!(
        include.join("ghostty.h").is_file(),
        "Set GHOSTTY_INCLUDE_DIR to the matching Ghostty header directory"
    );
    println!("cargo:rerun-if-changed=native/terminal.m");
    println!("cargo:rerun-if-changed=native/profile.h");
    println!("cargo:rerun-if-changed=native/terminal.h");
    cc::Build::new()
        .file("native/terminal.m")
        .include(include)
        .flag("-fno-objc-arc")
        .flag("-fblocks")
        .flag("-Wno-deprecated-declarations")
        .compile("riwork_ghostty_nsh_native");
    println!("cargo:rustc-link-search=native={}", library.display());
    println!("cargo:rustc-link-lib=static=ghostty-internal");
    println!("cargo:rustc-link-lib=c++");
    println!("cargo:rustc-link-lib=objc");
    for framework in [
        "AppKit",
        "Carbon",
        "CoreFoundation",
        "CoreGraphics",
        "CoreText",
        "CoreVideo",
        "Foundation",
        "IOSurface",
        "Metal",
        "QuartzCore",
    ] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
}
