//! Headless NSH test fixture writer. Requires an explicit empty scratch directory.
#[path = "../tests/support/mod.rs"]
mod support;

fn main() {
    let dir = std::env::args_os()
        .nth(1)
        .expect("usage: seed_fixture /tmp/explicit-fixture-directory");
    let dir = std::path::PathBuf::from(dir);
    assert!(
        dir.is_absolute()
            && (dir.starts_with("/tmp") || dir.starts_with("/private/tmp"))
            && !dir
                .components()
                .any(|part| part == std::path::Component::ParentDir),
        "fixture directory must be an absolute path inside /tmp"
    );
    let scratch = std::path::Path::new("/tmp")
        .canonicalize()
        .expect("/tmp exists");
    std::fs::create_dir_all(&dir).expect("create fixture directory");
    let dir = dir.canonicalize().expect("resolve fixture directory");
    assert!(
        dir.starts_with(scratch),
        "fixture directory must be inside /tmp"
    );
    assert!(
        !dir.join("layouts.json").exists(),
        "refusing to overwrite an existing fixture"
    );
    support::seed(&dir, true);
    println!(
        "GPUI LayoutStore fixture written: {} ({})",
        dir.display(),
        support::PROJECT
    );
}
