//! Opt-in automation transport for the integrated terminal verification binary.
//! This module is compiled only with `terminal-fixture`. A random isolated /tmp
//! RIWORK_HOME plus an explicit marker is required; the production binary has no
//! transport. Commands dispatch through the ordinary host/TSX lifecycle.
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::Duration,
};

const MARKER: &str = "riwork-terminal-fixture-v1";

pub struct Fixture {
    replies: mpsc::Sender<(u64, Value)>,
    root: PathBuf,
}
impl Fixture {
    /// The main thread only queues plain data. Serialization and file writes
    /// run in the transport thread, outside measured terminal disposal work.
    pub fn reply(&self, seq: u64, result: Value) {
        let _ = self.replies.send((seq, result));
    }
    /// Called only after AppView::run has ended and all native drains completed.
    pub fn finish(&self, snapshot: Value) -> Result<(), String> {
        fs::write(
            self.root.join("final-snapshot.json"),
            serde_json::to_vec_pretty(&snapshot).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
    }
}

pub fn start(home: PathBuf, dispatch: impl Fn(Value) + Send + 'static) -> Result<Fixture, String> {
    let home = home.canonicalize().map_err(|e| e.to_string())?;
    let temp = Path::new("/tmp")
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let root = home.join("terminal-fixture");
    if !home.starts_with(temp)
        || home == Path::new("/tmp")
        || fs::read_to_string(root.join("marker"))
            .map_err(|e| e.to_string())?
            .trim()
            != MARKER
    {
        return Err(
            "terminal fixture requires its explicit marker inside an isolated /tmp RIWORK_HOME"
                .into(),
        );
    }
    let requests = root.join("requests");
    let responses = root.join("responses");
    let processed = root.join("processed");
    fs::create_dir_all(&processed).map_err(|e| e.to_string())?;
    fs::create_dir_all(&requests).map_err(|e| e.to_string())?;
    fs::create_dir_all(&responses).map_err(|e| e.to_string())?;
    let (tx, rx) = mpsc::channel::<(u64, Value)>();
    thread::spawn(move || {
        let mut dispatched = BTreeSet::new();
        loop {
            while let Ok((seq, result)) = rx.try_recv() {
                let output = json!({"seq":seq,"result":result});
                let pending = responses.join(format!("{seq}.tmp"));
                if fs::write(&pending, output.to_string()).is_ok() {
                    let _ = fs::rename(&pending, responses.join(format!("{seq}.json")));
                }
            }
            let mut files = fs::read_dir(&requests)
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|entry| {
                    let path = entry.path();
                    if path.extension()?.to_str()? != "json" {
                        return None;
                    }
                    let seq = path.file_stem()?.to_str()?.parse::<u64>().ok()?;
                    Some((seq, path))
                })
                .collect::<Vec<_>>();
            files.sort_by_key(|(seq, _)| *seq);
            for (seq, path) in files {
                if dispatched.contains(&seq) {
                    continue;
                }
                let Ok(bytes) = fs::read(&path) else {
                    continue;
                };
                let Ok(mut command) = serde_json::from_slice::<Value>(&bytes) else {
                    continue;
                };
                if !command.is_object() {
                    continue;
                }
                command["seq"] = json!(seq);
                dispatched.insert(seq);
                let _ = fs::rename(&path, processed.join(format!("{seq}.json")));
                dispatch(command);
            }
            // Independent main-queue marker. No terminal tick or render occurs
            // here; the host records CLOCK_MONOTONIC at delivery.
            dispatch(json!({"op":"heartbeat"}));
            thread::sleep(Duration::from_millis(10));
        }
    });
    Ok(Fixture { replies: tx, root })
}
