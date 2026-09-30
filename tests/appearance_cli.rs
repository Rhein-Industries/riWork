//! `riwork appearance` against a throwaway RIWORK_HOME: the published colors
//! file as the phone companion reads it. The child never starts the GUI; a
//! regression that did is caught by the timeout and killed.

use serde_json::{Value, json};
use std::{
    fs,
    io::Read,
    path::PathBuf,
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

const NOT_PUBLISHED: &str =
    "riwork: RiWork has not published its appearance yet; open the RiWork app\n";

struct Home(PathBuf);

impl Home {
    fn new() -> Self {
        let path = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("riwork-appearance-cli-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn file(&self) -> PathBuf {
        self.0.join("appearance.json")
    }

    fn write(&self, text: &str) {
        fs::write(self.file(), text).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_riwork"))
            .args(args)
            .env("RIWORK_HOME", &self.0)
            .env("RIWORK_RUNTIME_DIR", self.0.join("runtime"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("riwork {args:?} did not exit; it may have started the GUI");
            }
            thread::sleep(Duration::from_millis(20));
        };
        let mut output = Output {
            status,
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        child
            .stdout
            .take()
            .unwrap()
            .read_to_end(&mut output.stdout)
            .unwrap();
        child
            .stderr
            .take()
            .unwrap()
            .read_to_end(&mut output.stderr)
            .unwrap();
        output
    }

    fn json(&self) -> Value {
        let output = self.run(&["appearance", "--json"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    /// The command fails without a word on stdout and with the publish message.
    fn assert_not_published(&self, args: &[&str]) {
        let output = self.run(args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            NOT_PUBLISHED,
            "{args:?}"
        );
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn document() -> Value {
    json!({
        "v": 1,
        "updated_at": 1_790_000_000u64,
        "dark": false,
        "palette": {
            "bg": "#fbf1c7", "panel": "#f4ebc1", "panel_active": "#ede5bb",
            "divider": "#d5cba1", "cyan": "#427b58", "magenta": "#8f3f71",
            "gold": "#8a5c00", "text": "#3c3836", "muted": "#665c54"
        },
        "terminal": {
            "background": "#fbf1c7",
            "foreground": "#3c3836",
            "palette": [
                "#fbf1c7", "#cc241d", "#98971a", "#d79921", "#458588", "#b16286",
                "#689d6a", "#7c6f64", "#928374", "#9d0006", "#79740e", "#b57614",
                "#076678", "#8f3f71", "#427b58", "#3c3836"
            ]
        }
    })
}

#[test]
fn a_missing_file_fails_with_the_publish_message_and_creates_nothing() {
    let home = Home::new();
    home.assert_not_published(&["appearance"]);
    home.assert_not_published(&["appearance", "--json"]);
    home.assert_not_published(&["--json", "appearance"]);
    assert_eq!(fs::read_dir(&home.0).unwrap().count(), 0);

    // An unusable home directory reads the same as an empty one.
    let output = Home::new().run(&["appearance"]);
    assert_eq!(String::from_utf8_lossy(&output.stderr), NOT_PUBLISHED);
    let gone = Home::new();
    fs::remove_dir_all(&gone.0).unwrap();
    gone.assert_not_published(&["appearance", "--json"]);
}

#[test]
fn an_invalid_file_fails_the_same_way() {
    let home = Home::new();
    let bad = |change: &dyn Fn(&mut Value)| {
        let mut value = document();
        change(&mut value);
        value.to_string()
    };
    let cases = [
        String::new(),
        "not json".to_owned(),
        "[]".to_owned(),
        bad(&|v| v["v"] = json!(2)),
        bad(&|v| v["dark"] = json!("no")),
        bad(&|v| v["palette"]["gold"] = json!("gold")),
        bad(&|v| v["palette"]["gold"] = json!("#fff")),
        bad(&|v| drop(v["palette"].as_object_mut().unwrap().remove("text"))),
        bad(&|v| drop(v["terminal"]["palette"].as_array_mut().unwrap().pop())),
        bad(&|v| {
            v["terminal"]["palette"]
                .as_array_mut()
                .unwrap()
                .push(json!("#000000"))
        }),
        bad(&|v| v["updated_at"] = json!(-5)),
    ];
    for text in &cases {
        home.write(text);
        home.assert_not_published(&["appearance", "--json"]);
        home.assert_not_published(&["appearance"]);
    }
    // A directory, not a file.
    fs::remove_file(home.file()).unwrap();
    fs::create_dir(home.file()).unwrap();
    home.assert_not_published(&["appearance", "--json"]);
}

#[test]
fn a_file_over_sixteen_kib_is_invalid() {
    let home = Home::new();
    let mut text = document().to_string();
    text.push_str(&" ".repeat(16 * 1024 - text.len()));
    assert_eq!(text.len(), 16 * 1024);
    home.write(&text);
    assert_eq!(home.json(), document());
    text.push('\n');
    home.write(&text);
    home.assert_not_published(&["appearance", "--json"]);
    home.assert_not_published(&["appearance"]);
    // Far past the cap: rejected without being read whole.
    home.write(&" ".repeat(4 * 1024 * 1024));
    home.assert_not_published(&["appearance", "--json"]);
}

#[test]
fn json_prints_the_validated_document_in_the_contract_shape() {
    let home = Home::new();
    home.write(&document().to_string());
    let printed = home.json();
    assert_eq!(printed, document());

    // Re-serialized: lowercase colors, contract fields only, terminal optional.
    let mut loud = document();
    loud["palette"]["bg"] = json!("#FBF1C7");
    loud["terminal"]["palette"][1] = json!("#CC241D");
    loud["from_the_future"] = json!(true);
    loud["palette"]["accent"] = json!("#123456");
    home.write(&loud.to_string());
    assert_eq!(home.json(), document());

    let mut without = document();
    without.as_object_mut().unwrap().remove("terminal");
    home.write(&without.to_string());
    let printed = home.json();
    assert_eq!(printed, without);
    assert!(printed.get("terminal").is_none());

    // The CLI reads and prints; it does not touch the file.
    let before = fs::read(home.file()).unwrap();
    home.json();
    assert_eq!(fs::read(home.file()).unwrap(), before);
    assert!(
        !home.0.join("runtime").exists(),
        "a GUI instance was registered"
    );
}

#[test]
fn the_summary_is_short_and_not_json() {
    let home = Home::new();
    home.write(&document().to_string());
    let output = home.run(&["appearance"]);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let text = String::from_utf8(output.stdout).unwrap();
    assert_eq!(text.lines().count(), 4, "{text}");
    assert!(text.starts_with("Appearance: light (updated 2026-09-21 14:13:20 UTC)\n"));
    for expected in [
        "bg #fbf1c7",
        "muted #665c54",
        "background #fbf1c7",
        "16 palette colors",
    ] {
        assert!(text.contains(expected), "{expected}: {text}");
    }
    assert!(serde_json::from_str::<Value>(&text).is_err());

    let mut without = document();
    without.as_object_mut().unwrap().remove("terminal");
    without["dark"] = json!(true);
    home.write(&without.to_string());
    let text = String::from_utf8(home.run(&["appearance"]).stdout).unwrap();
    assert!(text.starts_with("Appearance: dark "), "{text}");
    assert!(text.ends_with("Terminal:   colors unknown\n"), "{text}");
}

#[test]
fn unexpected_arguments_are_refused() {
    let home = Home::new();
    home.write(&document().to_string());
    for args in [
        &["appearance", "now"][..],
        &["appearance", "--json", "--watch"],
        &["appearance", "--bogus"],
    ] {
        let output = home.run(args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("Unexpected arguments"),
            "{args:?}"
        );
    }
    assert!(
        String::from_utf8(home.run(&["help"]).stdout)
            .unwrap()
            .contains("riwork appearance [--json]")
    );
}
