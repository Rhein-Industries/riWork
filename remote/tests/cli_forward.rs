//! Compile and exercise the very same root forwarding module without GPUI.
#[path = "../../src/remote_cli.rs"]
mod remote_cli;
#[test]
fn forwarding_child_entry() {
    if std::env::var_os("RIWORK_FORWARD_CHILD").is_some() {
        let args = vec![
            "start".into(),
            "--json".into(),
            "line with spaces; $(literal)".into(),
        ];
        match remote_cli::forward(&args) {
            Ok(()) => {}
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(23);
            }
        }
    }
}
#[cfg(unix)]
#[test]
fn root_forwarder_preserves_literal_arguments_home_and_child_exit_status() {
    use std::{fs, os::unix::fs::PermissionsExt, process::Command};
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("riwork-remote");
    let capture = tmp.path().join("capture");
    fs::write(&bin,"#!/bin/sh\nprintf '%s\\n' \"$RIWORK_HOME\" \"$RIWORK_CLI\" \"$@\" > \"$RIWORK_FORWARD_CAPTURE\"\nexit 7\n").unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o700)).unwrap();
    let exe = std::env::current_exe().unwrap();
    let output = Command::new(&exe)
        .args(["--exact", "forwarding_child_entry", "--nocapture"])
        .env("RIWORK_FORWARD_CHILD", "1")
        .env("RIWORK_REMOTE_BIN", &bin)
        .env("RIWORK_HOME", tmp.path())
        .env_remove("RIWORK_CLI")
        .env("RIWORK_FORWARD_CAPTURE", &capture)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7));
    let expected = format!(
        "{}\n{}\nstart\n--json\nline with spaces; $(literal)\n",
        tmp.path().display(),
        exe.display()
    );
    assert_eq!(fs::read_to_string(&capture).unwrap(), expected);
    let missing = Command::new(&exe)
        .args(["--exact", "forwarding_child_entry", "--nocapture"])
        .env("RIWORK_FORWARD_CHILD", "1")
        .env("RIWORK_REMOTE_BIN", tmp.path().join("missing"))
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(23));
    let diagnostic = String::from_utf8_lossy(&missing.stderr);
    assert!(
        diagnostic.contains("Missing standalone remote binary")
            && diagnostic.contains("remote/Cargo.toml")
            && diagnostic.contains("RIWORK_REMOTE_BIN")
    );
}

#[cfg(unix)]
#[test]
fn resolver_follows_path_links_and_finds_profile_packaged_companion() {
    use std::{fs, os::unix::fs::symlink};
    let tmp = tempfile::tempdir().unwrap();
    let profile = tmp.path().canonicalize().unwrap().join("target/debug");
    let macos = profile.join("RiWork.app/Contents/MacOS");
    fs::create_dir_all(&macos).unwrap();
    let root = profile.join("riwork");
    fs::write(&root, "CLI").unwrap();
    let link = tmp.path().join("riwork-on-path");
    symlink(&root, &link).unwrap();
    assert!(remote_cli::resolve(&link, None).is_err());
    let packed = macos.join("riwork-remote");
    fs::write(&packed, "companion").unwrap();
    assert_eq!(remote_cli::resolve(&link, None).unwrap(), packed);
    let packaged_cli = macos.join("riwork");
    fs::write(&packaged_cli, "CLI").unwrap();
    assert_eq!(remote_cli::resolve(&packaged_cli, None).unwrap(), packed);
    let sibling = profile.join("riwork-remote");
    fs::write(&sibling, "explicit companion").unwrap();
    assert_eq!(remote_cli::resolve(&link, None).unwrap(), sibling);
    assert_eq!(
        remote_cli::resolve(&link, Some(packed.clone())).unwrap(),
        packed
    );
    assert!(remote_cli::resolve(&link, Some(tmp.path().join("missing override"))).is_err());
}
