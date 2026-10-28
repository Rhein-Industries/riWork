//! Production NSH registration + unchanged production policy; dispatch is fake.
//! No driver, AppKit, window or input provider is constructed here.
#[allow(dead_code)]
#[path = "../../riwork-engine/src/cua/guard.rs"]
mod policy;
use super::*;
use serde_json::json;

#[test]
fn phase6c_nsh_host_registration_refuses_own_input_and_resolves_fixture_targets() {
    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let fixture =
        Scratch(std::env::temp_dir().join(format!("riwork-6c-guard-{}", uuid::Uuid::new_v4())));
    riwork_core::paths::create_private_dir(&fixture.0).unwrap();
    let home = fixture.0.join("home");
    fs::create_dir(&home).unwrap();
    let registry = fixture.0.join("runtime");
    let host = HostRuntime::register_with_manager(
        RuntimeManager::at(registry.clone()).unwrap(),
        &home,
        None,
    )
    .unwrap();
    let reader = RuntimeManager::at(registry).unwrap();
    let instances = reader.instances().unwrap();
    assert_eq!(instances.len(), 1);
    assert_eq!(instances[0].pid, std::process::id());
    assert_eq!(instances[0].state_home, home.canonicalize().unwrap());
    let own_pid = instances[0].pid;
    let other = own_pid + 100_000; // a number only, never looked up or sent input
    let mut dispatched = vec![];
    for (tool, arguments, refused) in [
        ("click", json!({"pid":own_pid,"window_id":1}), true),
        (
            "type_text",
            json!({"target":{"pid":own_pid.to_string()},"text":"fake"}),
            true,
        ),
        ("new_future_input_tool", json!({"pid":own_pid}), true),
        ("hotkey", json!({"keys":["META","Q"]}), true),
        ("click", json!({"pid":other,"scope":"desktop"}), true),
        ("get_window_state", json!({"pid":own_pid}), false),
        ("click", json!({"pid":other,"window_id":2}), false),
        (
            "type_text",
            json!({"target":{"pid":other},"text":"fake"}),
            false,
        ),
    ] {
        let verdict = policy::verdict(tool, &arguments, || {
            reader
                .instances()
                .unwrap()
                .iter()
                .map(|instance| instance.pid)
                .collect()
        });
        assert_eq!(
            matches!(verdict, policy::Verdict::Refuse(_)),
            refused,
            "{tool}: {verdict:?}"
        );
        if !refused {
            dispatched.push((tool, arguments));
        }
        println!(
            "fake dispatch {tool}: {}",
            if refused {
                "REFUSED (no dispatch)"
            } else {
                "resolved in fake sink"
            }
        );
    }
    assert_eq!(dispatched.len(), 3);
    assert_eq!(dispatched[1].1["pid"], other);
    drop(host);
    assert!(reader.instances().unwrap().is_empty());
    println!(
        "NSH registered test-process PID; all own inputs refused; fixture targets resolved; registry cleaned. No desktop interaction."
    );
}
