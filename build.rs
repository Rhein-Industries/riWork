//! Builds `riwork-speech`, the chat's dictation helper (macos/speech), next to the `riwork`
//! executable, where the app and `scripts/bundle-macos.sh` find it. It is Swift because Apple's
//! on-device `SpeechAnalyzer` is a Swift-only API, and it compiles the phone's vocabulary and
//! rewrite (ios/Core/SpeechVocabulary.swift) in place, so the Mac and the phone share them.
//!
//! Without a Swift compiler the build goes on and the chat says that dictation is not
//! available; a Swift compiler that fails the helper fails the build, so a broken helper is
//! never shipped silently.

use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

const SOURCES: [&str; 3] = [
    "macos/speech/main.swift",
    "macos/speech/Engines.swift",
    "ios/Core/SpeechVocabulary.swift",
];
const INFO_PLIST: &str = "macos/speech/Info.plist";

fn main() {
    for path in SOURCES.iter().chain([&INFO_PLIST]) {
        println!("cargo:rerun-if-changed={path}");
    }
    println!("cargo:rerun-if-env-changed=RIWORK_SKIP_SPEECH_HELPER");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos")
        || env::var_os("RIWORK_SKIP_SPEECH_HELPER").is_some()
    {
        return;
    }
    if !has_swiftc() {
        println!(
            "cargo:warning=No Swift compiler (xcrun swiftc): chat dictation will be unavailable"
        );
        return;
    }
    let arch = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        Ok("x86_64") => "x86_64",
        other => panic!("riwork-speech: unsupported architecture {other:?}"),
    };
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let helper = out.join("riwork-speech");
    let optimize = if env::var("PROFILE").as_deref() == Ok("release") {
        "-O"
    } else {
        "-Onone"
    };
    let status = Command::new("/usr/bin/xcrun")
        .args(["--sdk", "macosx", "swiftc"])
        .args([optimize, "-swift-version", "5", "-target"])
        .arg(format!("{arch}-apple-macos13.0"))
        // An Info.plist in the executable names it and says why it listens, should macOS ever
        // ask on its own behalf (a helper run by hand, outside RiWork.app).
        .args(["-Xlinker", "-sectcreate", "-Xlinker", "__TEXT", "-Xlinker"])
        .args(["__info_plist", "-Xlinker"])
        .arg(manifest.join(INFO_PLIST))
        .args(SOURCES.iter().map(|source| manifest.join(source)))
        .arg("-module-cache-path")
        .arg(out.join("module-cache"))
        .arg("-o")
        .arg(&helper)
        .status()
        .expect("riwork-speech: run swiftc");
    assert!(status.success(), "riwork-speech: swiftc failed ({status})");
    // OUT_DIR is <target>/<profile>/build/riwork-<hash>/out: put the helper beside `riwork`.
    if let Some(profile_dir) = out.ancestors().nth(3) {
        copy(&helper, &profile_dir.join("riwork-speech"));
    }
    println!(
        "cargo:rustc-env=RIWORK_SPEECH_HELPER_BUILT={}",
        helper.display()
    );
}

fn has_swiftc() -> bool {
    Command::new("/usr/bin/xcrun")
        .args(["--sdk", "macosx", "--find", "swiftc"])
        .output()
        .is_ok_and(|output| output.status.success())
}

fn copy(from: &Path, to: &Path) {
    // Replace rather than overwrite: a running app may have the old one open.
    let staged = to.with_extension("new");
    std::fs::copy(from, &staged).expect("riwork-speech: copy the helper");
    std::fs::rename(&staged, to).expect("riwork-speech: place the helper");
}
