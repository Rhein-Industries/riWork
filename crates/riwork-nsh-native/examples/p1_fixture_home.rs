//! cargo run -p riwork-nsh-native --features test-support --example p1_fixture_home --
//!   --scenario p1-basic --out /tmp/riwork-p1-unique/home
use std::path::PathBuf;

fn run() -> Result<(), String> {
    let mut args = std::env::args_os().skip(1);
    let (mut scenario, mut out, mut executable) = (None, None, None);
    while let Some(flag) = args.next() {
        let value = args.next().ok_or("Every flag requires a value")?;
        match flag.to_str() {
            Some("--scenario") => scenario = Some(value.to_string_lossy().into_owned()),
            Some("--out") => out = Some(PathBuf::from(value)),
            Some("--executable") => executable = Some(PathBuf::from(value)),
            _ => return Err(format!("Unknown option {}", flag.to_string_lossy())),
        }
    }
    let scenario = scenario.ok_or("Expected --scenario p1-basic or p1-restore")?;
    let out = out.ok_or("Expected --out /tmp/fresh-fixture/home")?;
    if scenario == "p1-restore" && executable.is_none() {
        // Cargo places examples in <profile>/examples, next to the host binary.
        executable = std::env::current_exe()
            .ok()
            .and_then(|path| path.parent()?.parent().map(|dir| dir.join("riwork")));
    }
    let manifest =
        riwork_nsh_native::testing::fixtures::generate(&scenario, &out, executable.as_deref())?;
    println!(
        "{}",
        serde_json::to_string_pretty(&manifest).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("p1_fixture_home: {error}");
        std::process::exit(2);
    }
}
