//! Only the pinned Ghostty C archive is linked; no Ghostty or GPUI Rust crate.
use sha2::{Digest, Sha256};
use std::{env, fs, io::Read, path::Path};

fn sha256(path: &Path) -> String {
    let mut file = fs::File::open(path)
        .unwrap_or_else(|error| panic!("Cannot read {}: {error}", path.display()));
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 65536];
    loop {
        let count = file
            .read(&mut buffer)
            .expect("read pinned Ghostty artifact");
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    format!("{:x}", hash.finalize())
}

fn directory_manifest(root: &Path) -> (String, usize) {
    fn collect(root: &Path, paths: &mut Vec<std::path::PathBuf>) {
        for entry in fs::read_dir(root).expect("read pinned Ghostty theme directory") {
            let entry = entry.expect("read Ghostty theme entry");
            let kind = entry.file_type().expect("Ghostty theme file type");
            assert!(
                !kind.is_symlink(),
                "Pinned themes must not contain symlinks"
            );
            if kind.is_dir() {
                collect(&entry.path(), paths);
            } else {
                assert!(kind.is_file(), "Pinned themes must be regular files");
                paths.push(entry.path());
            }
        }
    }
    let mut files = Vec::new();
    collect(root, &mut files);
    files.sort();
    let mut hash = Sha256::new();
    for path in &files {
        let name = path
            .strip_prefix(root)
            .unwrap()
            .to_str()
            .expect("UTF-8 theme name")
            .as_bytes();
        let bytes = fs::read(path).expect("read pinned theme");
        for data in [name, bytes.as_slice()] {
            hash.update((data.len() as u64).to_le_bytes());
            hash.update(data);
        }
    }
    (format!("{:x}", hash.finalize()), files.len())
}

fn main() {
    println!("cargo:rerun-if-env-changed=RIWORK_GHOSTTY_INSTALL_DIR");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_TERMINAL_FIXTURE");
    println!("cargo:rerun-if-changed=ghostty-pin.json");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let manifest = std::path::PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let install = env::var_os("RIWORK_GHOSTTY_INSTALL_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| manifest.join(".runtime/pinned/install"));
    let pin: serde_json::Value =
        serde_json::from_str(include_str!("ghostty-pin.json")).expect("Ghostty pin JSON");
    let provenance_path = install.join("provenance.json");
    let provenance_bytes = fs::read(&provenance_path).unwrap_or_else(|error| {
        panic!(
            "Pinned Ghostty has not been prepared at {} ({error}). Run \
             python3 crates/riwork-nsh-terminal/scripts/prepare_ghostty.py build \
             --zig /path/to/zig-0.16.0; see crates/riwork-nsh-terminal/BUILD.md. \
             No system or GPUI cache fallback is permitted.",
            install.display()
        )
    });
    let provenance: serde_json::Value =
        serde_json::from_slice(&provenance_bytes).expect("Ghostty provenance JSON");
    assert_eq!(provenance["schema_version"], 1, "Ghostty provenance schema");
    assert_eq!(provenance["revision"], pin["revision"], "Ghostty revision");
    assert_eq!(
        provenance["source_manifest_sha256"], pin["source_manifest"]["sha256"],
        "Ghostty vendor source manifest"
    );
    assert_eq!(
        provenance["source_distribution_sha256"], pin["source_distribution"]["sha256"],
        "Ghostty source distribution"
    );
    assert_eq!(provenance["zig_version"], pin["zig"]["version"], "Zig pin");
    match provenance["action"].as_str() {
        Some("import_exact_go_artifacts") => assert_eq!(
            provenance["artifacts"]["lib/libghostty-internal.a"],
            pin["gate_artifacts"]["lib/libghostty-internal.a"],
            "Imported archive must be the exact tested GO artifact"
        ),
        Some("built_from_verified_pinned_source") => assert_eq!(
            provenance["zig_binary_sha256"], pin["zig"]["macos_aarch64_binary_sha256"],
            "Rebuilt archive must use the pinned Zig binary"
        ),
        _ => panic!("Unrecognized Ghostty preparation provenance"),
    }
    assert_eq!(
        provenance["target"].as_str(),
        Some(env::var("TARGET").unwrap().as_str()),
        "Ghostty archive target does not match Cargo target"
    );
    for relative in ["lib/libghostty-internal.a", "include/ghostty.h"] {
        let path = install.join(relative);
        println!("cargo:rerun-if-changed={}", path.display());
        let expected = provenance["artifacts"][relative]
            .as_str()
            .expect("missing Ghostty artifact hash");
        assert_eq!(
            sha256(&path),
            expected,
            "Ghostty artifact changed: {relative}"
        );
    }
    assert_eq!(
        provenance["artifacts"]["include/ghostty.h"], pin["gate_artifacts"]["include/ghostty.h"],
        "Ghostty C API header does not match the frozen pin"
    );
    println!("cargo:rerun-if-changed={}", provenance_path.display());
    let resources = install.join("share/ghostty");
    let themes = resources.join("themes");
    let (themes_hash, theme_count) = directory_manifest(&themes);
    assert_eq!(
        themes_hash,
        pin["themes"]["source_manifest_sha256"].as_str().unwrap(),
        "Pinned theme manifest"
    );
    assert_eq!(
        theme_count as u64,
        pin["themes"]["file_count"].as_u64().unwrap(),
        "Pinned theme count"
    );
    println!("cargo:rerun-if-changed={}", themes.display());
    let resource_path = resources
        .to_str()
        .expect("Ghostty resources path must be UTF-8");
    assert!(
        !resource_path.contains(['\n', '\r']),
        "Invalid Ghostty resources path"
    );
    println!("cargo:rustc-env=RIWORK_GHOSTTY_RESOURCES_DIR={resource_path}");
    let resource_literal = serde_json::to_string(resource_path).unwrap();
    println!("cargo:rerun-if-changed=native");
    let mut native = cc::Build::new();
    if env::var_os("CARGO_FEATURE_TERMINAL_FIXTURE").is_some() {
        native.define("RIWORK_TERMINAL_FIXTURE", "1");
    }
    native
        .file("native/terminal.m")
        .include(install.join("include"))
        .define("RIWORK_GHOSTTY_RESOURCES_DIR", resource_literal.as_str())
        .flag("-fno-objc-arc")
        .flag("-fblocks")
        .flag("-Wno-deprecated-declarations")
        .compile("riwork_nsh_terminal_native");
    println!(
        "cargo:rustc-link-search=native={}",
        install.join("lib").display()
    );
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
