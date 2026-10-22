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
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let repository = manifest
        .parent()
        .and_then(Path::parent)
        .expect("riwork-desktop must live under crates/");
    for path in SOURCES.iter().chain([&INFO_PLIST]) {
        println!("cargo:rerun-if-changed={}", repository.join(path).display());
    }
    println!("cargo:rerun-if-env-changed=RIWORK_SKIP_SPEECH_HELPER");
    println!(
        "cargo:rerun-if-changed={}",
        repository.join("macos/diagram/main.swift").display()
    );
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    if !has_swiftc() {
        println!(
            "cargo:warning=No Swift compiler (xcrun swiftc): chat dictation and diagram rendering will be unavailable"
        );
        return;
    }
    let arch = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        Ok("x86_64") => "x86_64",
        other => panic!("riwork-speech: unsupported architecture {other:?}"),
    };
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let module_cache = prepare_swift_module_cache(&out)
        .expect("riwork-speech: prepare a compatible Swift module cache");
    let helper = out.join("riwork-speech");
    let optimize = if env::var("PROFILE").as_deref() == Ok("release") {
        "-O"
    } else {
        "-Onone"
    };
    let diagram = out.join("riwork-diagram");
    let status = Command::new("/usr/bin/xcrun")
        .args([
            "--sdk",
            "macosx",
            "swiftc",
            optimize,
            "-swift-version",
            "5",
            "-target",
        ])
        .arg(format!("{arch}-apple-macos13.0"))
        .arg(repository.join("macos/diagram/main.swift"))
        .arg("-module-cache-path")
        .arg(&module_cache)
        .arg("-o")
        .arg(&diagram)
        .status()
        .expect("riwork-diagram: run swiftc");
    assert!(status.success(), "riwork-diagram: swiftc failed ({status})");
    if let Some(profile_dir) = out.ancestors().nth(3) {
        copy(&diagram, &profile_dir.join("riwork-diagram"));
    }
    println!(
        "cargo:rustc-env=RIWORK_DIAGRAM_HELPER_BUILT={}",
        diagram.display()
    );
    if env::var_os("RIWORK_SKIP_SPEECH_HELPER").is_some() {
        return;
    }
    let status = Command::new("/usr/bin/xcrun")
        .args(["--sdk", "macosx", "swiftc"])
        .args([optimize, "-swift-version", "5", "-target"])
        .arg(format!("{arch}-apple-macos13.0"))
        // An Info.plist in the executable names it and says why it listens, should macOS ever
        // ask on its own behalf (a helper run by hand, outside RiWork.app).
        .args(["-Xlinker", "-sectcreate", "-Xlinker", "__TEXT", "-Xlinker"])
        .args(["__info_plist", "-Xlinker"])
        .arg(repository.join(INFO_PLIST))
        .args(SOURCES.iter().map(|source| repository.join(source)))
        .arg("-module-cache-path")
        .arg(&module_cache)
        .arg("-o")
        .arg(&helper)
        .status()
        .expect("riwork-speech: run swiftc");
    assert!(status.success(), "riwork-speech: swiftc failed ({status})");
    // OUT_DIR is <target>/<profile>/build/riwork-desktop-<hash>/out: put the helper beside `riwork`.
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

/// Swift/Clang precompiled modules contain absolute cache paths. The updater
/// clones Cargo's build outputs into a private staging target, so those modules
/// must be rebuilt after relocation. Keep the cache on ordinary in-place builds.
fn prepare_swift_module_cache(out: &Path) -> std::io::Result<PathBuf> {
    let out = out.canonicalize()?;
    let cache = out.join("module-cache");
    let marker = out.join("swift-module-cache.location");
    let location = cache.as_os_str().as_encoded_bytes();
    let compatible = std::fs::read(&marker).is_ok_and(|previous| previous == location)
        && std::fs::symlink_metadata(&cache).is_ok_and(|metadata| metadata.is_dir());
    if !compatible {
        // Only this fixed child of Cargo's generated OUT_DIR is removed. The
        // marker's contents are compared as bytes, never used as a deletion path.
        match std::fs::symlink_metadata(&cache) {
            Ok(metadata) if metadata.is_dir() => std::fs::remove_dir_all(&cache)?,
            Ok(_) => std::fs::remove_file(&cache)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        std::fs::create_dir(&cache)?;
        std::fs::write(marker, location)?;
    }
    Ok(cache)
}

fn copy(from: &Path, to: &Path) {
    // Replace rather than overwrite: a running app may have the old one open.
    let staged = to.with_extension("new");
    std::fs::copy(from, &staged).expect("riwork-speech: copy the helper");
    std::fs::rename(&staged, to).expect("riwork-speech: place the helper");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = env::temp_dir().join(format!(
                "riwork-swift-cache-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn in_place_build_preserves_compatible_precompiled_modules() {
        let root = Scratch::new();
        let cache = prepare_swift_module_cache(&root.0).unwrap();
        std::fs::write(cache.join("SwiftShims.pcm"), "compatible").unwrap();
        assert_eq!(prepare_swift_module_cache(&root.0).unwrap(), cache);
        assert_eq!(
            std::fs::read_to_string(cache.join("SwiftShims.pcm")).unwrap(),
            "compatible"
        );
    }

    #[test]
    fn a_cloned_marker_discards_only_relocated_generated_modules() {
        let root = Scratch::new();
        let old = root.0.join("old");
        let staged = root.0.join("staged");
        std::fs::create_dir(&old).unwrap();
        std::fs::create_dir(&staged).unwrap();
        prepare_swift_module_cache(&old).unwrap();
        std::fs::create_dir(staged.join("module-cache")).unwrap();
        std::fs::write(
            staged.join("module-cache/SwiftShims.pcm"),
            "old absolute path",
        )
        .unwrap();
        std::fs::copy(
            old.join("swift-module-cache.location"),
            staged.join("swift-module-cache.location"),
        )
        .unwrap();
        std::fs::write(staged.join("riwork-speech"), "keep helper").unwrap();
        let cache = prepare_swift_module_cache(&staged).unwrap();
        assert!(!cache.join("SwiftShims.pcm").exists());
        assert_eq!(
            std::fs::read(staged.join("swift-module-cache.location")).unwrap(),
            cache.as_os_str().as_encoded_bytes()
        );
        assert_eq!(
            std::fs::read_to_string(staged.join("riwork-speech")).unwrap(),
            "keep helper"
        );
    }

    #[test]
    fn a_legacy_cache_without_a_marker_is_rebuilt() {
        let root = Scratch::new();
        std::fs::create_dir(root.0.join("module-cache")).unwrap();
        std::fs::write(root.0.join("module-cache/old.pcm"), "legacy").unwrap();
        assert!(
            !prepare_swift_module_cache(&root.0)
                .unwrap()
                .join("old.pcm")
                .exists()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_generated_cache_symlink_cannot_remove_external_modules() {
        let root = Scratch::new();
        let outside = Scratch::new();
        std::fs::write(outside.0.join("keep.pcm"), "outside").unwrap();
        std::os::unix::fs::symlink(&outside.0, root.0.join("module-cache")).unwrap();
        let cache = prepare_swift_module_cache(&root.0).unwrap();
        assert!(
            !std::fs::symlink_metadata(cache)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(outside.0.join("keep.pcm").exists());
    }
}
