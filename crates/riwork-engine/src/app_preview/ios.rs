//! An isolated iOS Simulator lifecycle and its windowless native stream bridge.
//!
//! Every mutation addresses the device UUID returned by this launch's `create`.
//! Existing devices and the global `booted` alias are deliberately never used.

use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    io::{Read, Write},
    os::unix::{fs::DirBuilderExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use super::{
    IosPreviewSpec, OwnedProcess, PreviewLog, PreviewPhase, PreviewSpec, PreviewState,
    cooperative::Endpoint, push_log, set_message,
};
use serde_json::Value;

const OUTPUT_LIMIT: usize = 4 * 1024 * 1024;
const METADATA_TIMEOUT: Duration = Duration::from_secs(60);
const BUILD_TIMEOUT: Duration = Duration::from_secs(20 * 60);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimulatorSelection {
    pub device_type: String,
    pub device_name: String,
    pub runtime: String,
    pub runtime_name: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct IosPreviewStatus {
    pub udid: String,
    pub device_name: String,
    pub runtime_name: String,
    pub bundle_id: Option<String>,
    pub app_pid: Option<u32>,
}

/// Owned for the entire preview lifetime. Nothing else may manufacture a lease
/// around an existing simulator UUID.
pub(super) struct Session {
    udid: String,
    lease_path: Option<PathBuf>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Lease {
    owner_pid: u32,
    udid: String,
    device_name: String,
}

impl Session {
    fn record(udid: String, device_name: String, cache: &Path) -> Result<Self, String> {
        let mut session = Self {
            udid,
            lease_path: None,
        };
        let directory = cache.join("leases");
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&directory)
            .map_err(|error| error.to_string())?;
        let lease_path = directory.join(format!("{}.json", session.udid));
        session.lease_path = Some(lease_path.clone());
        let lease = Lease {
            owner_pid: std::process::id(),
            udid: session.udid.clone(),
            device_name,
        };
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lease_path)
            .map_err(|error| format!("Cannot record preview simulator ownership: {error}"))?;
        file.write_all(&serde_json::to_vec(&lease).map_err(|error| error.to_string())?)
            .and_then(|()| file.sync_all())
            .map_err(|error| error.to_string())?;
        Ok(session)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Keep cleanup on this worker, bounded and restricted to this lease.
        for verb in ["shutdown", "delete"] {
            let mut command = Command::new("/usr/bin/xcrun");
            command.args(["simctl", verb, &self.udid]);
            let deleted = cleanup_command(command, Duration::from_secs(20)).is_ok();
            if verb == "delete" && deleted {
                if let Some(path) = &self.lease_path {
                    let _ = fs::remove_file(path);
                }
            }
        }
    }
}

struct Runner {
    state: Arc<Mutex<PreviewState>>,
    cancellation: Arc<AtomicU64>,
    revision: u64,
}

impl Runner {
    fn check_cancelled(&self) -> Result<(), String> {
        if self.cancellation.load(Ordering::SeqCst) != self.revision {
            Err("iOS preview startup was cancelled.".into())
        } else {
            Ok(())
        }
    }

    fn stage(&self, message: &str) -> Result<(), String> {
        self.check_cancelled()?;
        set_message(&self.state, PreviewPhase::Starting, message);
        push_log(
            &mut self.state.lock().unwrap_or_else(|e| e.into_inner()).logs,
            PreviewLog {
                text: format!("\n{message}\n"),
                stderr: false,
            },
        );
        Ok(())
    }

    fn xcrun(&self, args: &[String], cwd: &Path, timeout: Duration) -> Result<String, String> {
        let mut command = Command::new("/usr/bin/xcrun");
        command.args(args).current_dir(cwd);
        self.run(command, timeout, true)
    }

    fn run(
        &self,
        mut command: Command,
        timeout: Duration,
        cancellable: bool,
    ) -> Result<String, String> {
        self.check_cancelled()?;
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut child = command.spawn().map_err(|error| format!("Cannot start Xcode tool: {error}. Install Xcode and select it in Xcode Settings → Locations."))?;
        let group = child.id();
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut readers = Vec::new();
        for (source, stderr) in [
            (
                child
                    .stdout
                    .take()
                    .map(|s| Box::new(s) as Box<dyn Read + Send>),
                false,
            ),
            (
                child
                    .stderr
                    .take()
                    .map(|s| Box::new(s) as Box<dyn Read + Send>),
                true,
            ),
        ] {
            if let Some(mut source) = source {
                let state = Arc::clone(&self.state);
                let output = Arc::clone(&output);
                readers.push(thread::spawn(move || {
                    let mut bytes = [0; 8192];
                    while let Ok(count) = source.read(&mut bytes) {
                        if count == 0 {
                            break;
                        }
                        push_log(
                            &mut state.lock().unwrap_or_else(|e| e.into_inner()).logs,
                            PreviewLog {
                                text: String::from_utf8_lossy(&bytes[..count]).into_owned(),
                                stderr,
                            },
                        );
                        if !stderr {
                            let mut output = output.lock().unwrap_or_else(|e| e.into_inner());
                            let room = OUTPUT_LIMIT.saturating_sub(output.len());
                            output.extend_from_slice(&bytes[..count.min(room)]);
                        }
                    }
                }));
            }
        }
        let mut process = OwnedProcess {
            child,
            group,
            reaped: false,
        };
        let deadline = Instant::now() + timeout;
        loop {
            if cancellable && self.check_cancelled().is_err() {
                process.stop();
                for reader in readers {
                    let _ = reader.join();
                }
                return Err("iOS preview startup was cancelled.".into());
            }
            if Instant::now() >= deadline {
                process.stop();
                for reader in readers {
                    let _ = reader.join();
                }
                return Err(format!(
                    "Xcode tool timed out after {} seconds. See preview logs.",
                    timeout.as_secs()
                ));
            }
            if process.exited().is_some() {
                // The unreaped leader reserves this launch's group number.
                // Close inherited pipes held by any surviving descendants
                // before joining readers, without risking a recycled group.
                unsafe {
                    libc::kill(-(process.group as libc::pid_t), libc::SIGKILL);
                }
                let status = process.child.wait().map_err(|error| error.to_string())?;
                process.reaped = true;
                for reader in readers {
                    let _ = reader.join();
                }
                let bytes = output.lock().unwrap_or_else(|e| e.into_inner());
                if !status.success() {
                    return Err(format!(
                        "Xcode tool failed ({status}). See preview logs for the build or simulator error."
                    ));
                }
                return Ok(String::from_utf8_lossy(&bytes).into_owned());
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
}

pub(super) fn launch(
    spec: &PreviewSpec,
    endpoint: &Endpoint,
    state: &Arc<Mutex<PreviewState>>,
    cancellation: Arc<AtomicU64>,
    revision: u64,
) -> Result<(OwnedProcess, Session), String> {
    #[cfg(not(target_os = "macos"))]
    return Err(
        "iOS previews require macOS with Xcode and an installed iOS Simulator runtime.".into(),
    );
    #[cfg(target_os = "macos")]
    {
        let ios = spec
            .ios
            .as_ref()
            .ok_or("Configure the Xcode scheme for this iOS preview.")?;
        let runner = Runner {
            state: Arc::clone(state),
            cancellation,
            revision,
        };
        runner.stage("Checking Xcode and installed iOS simulator runtimes…")?;
        let inventory = runner.xcrun(
            &strings(&["simctl", "list", "--json"]),
            &spec.cwd,
            METADATA_TIMEOUT,
        )?;
        let inventory: Value = serde_json::from_str(&inventory)
            .map_err(|error| format!("Cannot read simulator inventory: {error}"))?;
        let selection = select_simulator(&inventory, ios)?;
        let cache = cache_root()?;
        cleanup_stale_sessions(&cache, &inventory, &runner)?;
        let helper = ensure_bridge(&runner, &cache, &spec.cwd)?;
        runner.stage("Checking the Xcode simulator streaming adapter…")?;
        let mut probe = Command::new(&helper);
        probe.arg("--check-support").current_dir(&spec.cwd);
        runner.run(probe, METADATA_TIMEOUT, true)?;
        let project = resolve_project(&spec.cwd, ios.project.as_deref())?;
        let project_args = project_arguments(&project);
        let scheme = resolve_scheme(&runner, &spec.cwd, &project_args, &ios.scheme)?;
        runner.stage(&format!(
            "Creating a RiWork-owned {} ({})…",
            selection.device_name, selection.runtime_name
        ))?;
        let mut create = Command::new("/usr/bin/xcrun");
        let device_name = format!("RiWork Preview {}", uuid::Uuid::new_v4().simple());
        create
            .args([
                "simctl",
                "create",
                &device_name,
                &selection.device_type,
                &selection.runtime,
            ])
            .current_dir(&spec.cwd);
        // Complete create before honoring cancellation so its returned UUID can
        // always be put into the cleanup lease, even if Stop arrives mid-create.
        let udid = runner
            .run(create, Duration::from_secs(30), false)?
            .trim()
            .to_owned();
        if uuid::Uuid::parse_str(&udid).is_err() {
            return Err("simctl create returned an invalid device UUID.".into());
        }
        let session = Session::record(udid, device_name, &cache)?;
        state.lock().unwrap_or_else(|e| e.into_inner()).ios = Some(IosPreviewStatus {
            udid: session.udid.clone(),
            device_name: selection.device_name,
            runtime_name: selection.runtime_name,
            bundle_id: None,
            app_pid: None,
        });
        runner.check_cancelled()?;
        runner.stage("Booting the preview simulator…")?;
        runner.xcrun(
            &strings(&["simctl", "boot", &session.udid]),
            &spec.cwd,
            METADATA_TIMEOUT,
        )?;
        runner.xcrun(
            &strings(&["simctl", "bootstatus", &session.udid, "-b"]),
            &spec.cwd,
            Duration::from_secs(180),
        )?;
        let mut hash = DefaultHasher::new();
        (project.clone(), scheme.clone(), ios.configuration.clone()).hash(&mut hash);
        let build_key = format!("build-{:016x}", hash.finish());
        let derived_data = cache.join(&build_key);
        let _build_lock = cache_lock(
            &cache.join(format!("{build_key}.lock")),
            &runner,
            BUILD_TIMEOUT,
        )?;
        let mut build_args = strings(&["xcodebuild"]);
        build_args.extend(project_args);
        build_args.extend(strings(&[
            "-scheme",
            &scheme,
            "-configuration",
            if ios.configuration.trim().is_empty() {
                "Debug"
            } else {
                &ios.configuration
            },
            "-sdk",
            "iphonesimulator",
            "-destination",
            &format!("id={}", session.udid),
            "-derivedDataPath",
        ]));
        build_args.push(derived_data.to_string_lossy().into_owned());
        build_args.push("CODE_SIGNING_ALLOWED=NO".into());
        runner.stage(&format!("Building {scheme} for the iOS simulator…"))?;
        let mut build = build_args.clone();
        build.push("-quiet".into());
        build.push("build".into());
        runner.xcrun(&build, &spec.cwd, BUILD_TIMEOUT)?;
        let mut settings = build_args;
        settings.extend(strings(&["-showBuildSettings", "-json"]));
        let settings = runner.xcrun(&settings, &spec.cwd, METADATA_TIMEOUT)?;
        let (app, bundle_id) = application_product(
            &serde_json::from_str(&settings)
                .map_err(|error| format!("Cannot read Xcode build settings: {error}"))?,
            ios.bundle_id.as_deref(),
        )?;
        if !app.is_dir() {
            return Err(format!(
                "Xcode did not produce the application at {}.",
                app.display()
            ));
        }
        runner.stage(&format!("Installing and launching {bundle_id}…"))?;
        runner.xcrun(
            &strings(&["simctl", "install", &session.udid, &app.to_string_lossy()]),
            &spec.cwd,
            METADATA_TIMEOUT,
        )?;
        let launched = runner.xcrun(
            &strings(&[
                "simctl",
                "launch",
                "--terminate-running-process",
                &session.udid,
                &bundle_id,
            ]),
            &spec.cwd,
            METADATA_TIMEOUT,
        )?;
        if let Some(status) = &mut state.lock().unwrap_or_else(|e| e.into_inner()).ios {
            status.bundle_id = Some(bundle_id);
            status.app_pid = launched
                .trim()
                .rsplit_once(':')
                .and_then(|(_, pid)| pid.trim().parse().ok());
        }
        runner.stage("Starting live iOS video and touch input…")?;
        runner.check_cancelled()?;
        let mut command = Command::new(helper);
        command
            .args(["--udid", &session.udid])
            .current_dir(&spec.cwd);
        let process = OwnedProcess::launch_command(command, Some(endpoint), state)?;
        Ok((process, session))
    }
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| (*s).into()).collect()
}

fn version_tuple(version: &str) -> Vec<u32> {
    version.split('.').map(|n| n.parse().unwrap_or(0)).collect()
}

/// Resolve names against the installed inventory, ignoring unavailable runtimes
/// and incompatible devices rather than starting a random preexisting device.
pub fn select_simulator(
    inventory: &Value,
    spec: &IosPreviewSpec,
) -> Result<SimulatorSelection, String> {
    let runtimes = inventory
        .get("runtimes")
        .and_then(Value::as_array)
        .ok_or("Xcode returned no simulator runtime inventory.")?;
    let mut runtimes: Vec<&Value> = runtimes
        .iter()
        .filter(|r| {
            r.get("isAvailable").and_then(Value::as_bool) == Some(true)
                && r["identifier"]
                    .as_str()
                    .is_some_and(|id| id.starts_with("com.apple.CoreSimulator.SimRuntime.iOS-"))
        })
        .collect();
    if let Some(requested) = spec.runtime.as_deref().filter(|s| !s.trim().is_empty()) {
        runtimes.retain(|r| {
            r["identifier"].as_str() == Some(requested)
                || r["name"].as_str() == Some(requested)
                || r["version"].as_str() == Some(requested)
        });
    }
    runtimes.sort_by_key(|r| {
        std::cmp::Reverse(version_tuple(r["version"].as_str().unwrap_or_default()))
    });
    let types = inventory
        .get("devicetypes")
        .and_then(Value::as_array)
        .ok_or("Xcode returned no simulator device types.")?;
    for runtime in runtimes {
        let version = version_tuple(runtime["version"].as_str().unwrap_or_default());
        let encoded = (version.first().copied().unwrap_or(0) << 16)
            | (version.get(1).copied().unwrap_or(0) << 8)
            | version.get(2).copied().unwrap_or(0);
        let requested = spec.device_type.as_deref().filter(|s| !s.trim().is_empty());
        let mut devices: Vec<&Value> = types
            .iter()
            .filter(|d| {
                let matches = if let Some(requested) = requested {
                    d["identifier"].as_str() == Some(requested)
                        || d["name"].as_str() == Some(requested)
                } else {
                    d["name"]
                        .as_str()
                        .is_some_and(|name| name.starts_with("iPhone "))
                };
                let min = d["minRuntimeVersion"].as_u64().unwrap_or(0);
                let max = d["maxRuntimeVersion"].as_u64().unwrap_or(u32::MAX as u64);
                matches && (min..=max).contains(&(encoded as u64))
            })
            .collect();
        devices.sort_by_key(|d| {
            let name = d["name"].as_str().unwrap_or_default();
            let generation = name
                .split_whitespace()
                .nth(1)
                .and_then(|n| n.parse::<u32>().ok())
                .unwrap_or(0);
            (
                std::cmp::Reverse(generation),
                name.contains("Max"),
                name.contains("Pro"),
                name.to_owned(),
            )
        });
        if let Some(device) = devices.first() {
            return Ok(SimulatorSelection {
                device_type: device["identifier"].as_str().unwrap_or_default().into(),
                device_name: device["name"].as_str().unwrap_or_default().into(),
                runtime: runtime["identifier"].as_str().unwrap_or_default().into(),
                runtime_name: runtime["name"].as_str().unwrap_or_default().into(),
            });
        }
    }
    Err("No installed, available iOS runtime supports the selected device. Install an iOS Simulator runtime in Xcode Settings → Components, or choose another device/runtime.".into())
}

fn resolve_project(cwd: &Path, requested: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(requested) = requested.filter(|p| !p.as_os_str().is_empty()) {
        let project = if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            cwd.join(requested)
        };
        if !project.is_dir()
            || !matches!(
                project.extension().and_then(|s| s.to_str()),
                Some("xcodeproj" | "xcworkspace")
            )
        {
            return Err(
                "Choose an existing .xcodeproj or .xcworkspace for the iOS preview.".into(),
            );
        }
        return project.canonicalize().map_err(|e| e.to_string());
    }
    let entries = fs::read_dir(cwd).map_err(|e| e.to_string())?;
    let projects: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && matches!(
                    p.extension().and_then(|s| s.to_str()),
                    Some("xcodeproj" | "xcworkspace")
                )
        })
        .collect();
    let workspaces: Vec<_> = projects
        .iter()
        .filter(|p| p.extension().is_some_and(|e| e == "xcworkspace"))
        .collect();
    let candidates: Vec<_> = if workspaces.is_empty() {
        projects.iter().collect()
    } else {
        workspaces
    };
    if candidates.len() != 1 {
        return Err("Set the .xcodeproj or .xcworkspace path; the preview folder does not contain exactly one Xcode project/workspace.".into());
    }
    candidates[0].canonicalize().map_err(|e| e.to_string())
}

fn project_arguments(project: &Path) -> Vec<String> {
    vec![
        if project.extension().is_some_and(|e| e == "xcworkspace") {
            "-workspace"
        } else {
            "-project"
        }
        .into(),
        project.to_string_lossy().into_owned(),
    ]
}

fn resolve_scheme(
    runner: &Runner,
    cwd: &Path,
    project: &[String],
    requested: &str,
) -> Result<String, String> {
    let mut args = strings(&["xcodebuild", "-list", "-json"]);
    args.extend_from_slice(project);
    let output = runner.xcrun(&args, cwd, METADATA_TIMEOUT)?;
    let value: Value =
        serde_json::from_str(&output).map_err(|e| format!("Cannot read Xcode schemes: {e}"))?;
    let schemes: Vec<&str> = value
        .get("workspace")
        .or_else(|| value.get("project"))
        .and_then(|v| v.get("schemes"))
        .and_then(Value::as_array)
        .map(|v| v.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if !requested.trim().is_empty() {
        return if schemes.contains(&requested) {
            Ok(requested.into())
        } else {
            Err(format!(
                "Xcode scheme {requested:?} is not available. Available schemes: {}. Share the app scheme in Xcode.",
                schemes.join(", ")
            ))
        };
    }
    if schemes.len() == 1 {
        Ok(schemes[0].into())
    } else {
        Err(format!(
            "Choose the app's shared Xcode scheme. Available schemes: {}.",
            schemes.join(", ")
        ))
    }
}

fn application_product(
    settings: &Value,
    requested: Option<&str>,
) -> Result<(PathBuf, String), String> {
    let targets = settings
        .as_array()
        .ok_or("Xcode build settings were not an array.")?;
    let mut apps = Vec::new();
    for target in targets {
        let build = &target["buildSettings"];
        if build["PRODUCT_TYPE"].as_str() != Some("com.apple.product-type.application") {
            continue;
        }
        let Some(bundle) = build["PRODUCT_BUNDLE_IDENTIFIER"].as_str() else {
            continue;
        };
        if requested.is_some_and(|id| !id.is_empty() && id != bundle) {
            continue;
        }
        let Some(directory) = build["TARGET_BUILD_DIR"].as_str() else {
            continue;
        };
        let Some(name) = build["FULL_PRODUCT_NAME"].as_str() else {
            continue;
        };
        if !name.ends_with(".app") || Path::new(name).components().count() != 1 {
            continue;
        }
        apps.push((PathBuf::from(directory).join(name), bundle.into()));
    }
    if apps.len() != 1 {
        return Err("Build must produce exactly one iOS application; set its bundle ID if the scheme builds multiple apps.".into());
    }
    Ok(apps.remove(0))
}

fn cache_root() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("Cannot determine the user home for the iOS build cache.")?;
    let cache = home.join("Library/Caches/riwork/ios-preview");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&cache)
        .map_err(|e| format!("Cannot create iOS preview cache: {e}"))?;
    let metadata = fs::symlink_metadata(&cache).map_err(|e| e.to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("The iOS preview cache is not a real directory.".into());
    }
    Ok(cache)
}

/// Crash recovery is limited to a durable ownership record whose exact UUID
/// and generated name still match the device inventory and whose owner died.
/// We never infer ownership from a name prefix or a currently booted device.
fn cleanup_stale_sessions(cache: &Path, inventory: &Value, runner: &Runner) -> Result<(), String> {
    let Ok(entries) = fs::read_dir(cache.join("leases")) else {
        return Ok(());
    };
    let mut cleaned = 0;
    for entry in entries.filter_map(Result::ok).take(256) {
        runner.check_cancelled()?;
        let path = entry.path();
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !metadata.is_file() || metadata.len() > 8192 {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else { continue };
        let Ok(lease) = serde_json::from_slice::<Lease>(&bytes) else {
            continue;
        };
        if !stale_lease_matches(&lease, inventory) {
            continue;
        }
        let exists = inventory
            .get("devices")
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(|sets| sets.values())
            .filter_map(Value::as_array)
            .flatten()
            .any(|device| device["udid"].as_str() == Some(&lease.udid));
        if !exists {
            let _ = fs::remove_file(path);
            continue;
        }
        runner.stage("Removing a simulator left by a previous RiWork process…")?;
        drop(Session {
            udid: lease.udid,
            lease_path: Some(path),
        });
        cleaned += 1;
        if cleaned == 3 {
            break;
        }
    }
    Ok(())
}

fn stale_lease_matches(lease: &Lease, inventory: &Value) -> bool {
    if lease.owner_pid == 0
        || lease.owner_pid > i32::MAX as u32
        || uuid::Uuid::parse_str(&lease.udid).is_err()
        || !lease.device_name.starts_with("RiWork Preview ")
    {
        return false;
    }
    // SAFETY: signal zero only queries whether this process identity exists.
    if unsafe { libc::kill(lease.owner_pid as libc::pid_t, 0) } == 0
        || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    {
        return false;
    }
    inventory
        .get("devices")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|sets| sets.values())
        .filter_map(Value::as_array)
        .flatten()
        .find(|device| device["udid"].as_str() == Some(&lease.udid))
        .is_none_or(|device| device["name"].as_str() == Some(&lease.device_name))
}

#[cfg(target_os = "macos")]
fn ensure_bridge(runner: &Runner, cache: &Path, cwd: &Path) -> Result<PathBuf, String> {
    let source = include_str!("../../../../tools/ios-preview-bridge/main.m");
    let developer = runner.xcrun(
        &strings(&["--sdk", "macosx", "--find", "clang"]),
        cwd,
        METADATA_TIMEOUT,
    )?;
    let mut hash = DefaultHasher::new();
    (source, developer).hash(&mut hash);
    let build = cache.join(format!("bridge-{:016x}", hash.finish()));
    if build.join("bridge").is_file() {
        return Ok(build.join("bridge"));
    }
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&build)
        .or_else(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                Ok(())
            } else {
                Err(e)
            }
        })
        .map_err(|e| e.to_string())?;
    let _compile_lock = cache_lock(&build.join("compile.lock"), runner, METADATA_TIMEOUT)?;
    if build.join("bridge").is_file() {
        return Ok(build.join("bridge"));
    }
    let source_path = build.join("main.m");
    fs::write(&source_path, source).map_err(|e| e.to_string())?;
    let temporary = build.join(format!("bridge-{}", uuid::Uuid::new_v4().simple()));
    runner.stage("Compiling the installed Xcode simulator streaming adapter…")?;
    runner.xcrun(
        &strings(&[
            "--sdk",
            "macosx",
            "clang",
            "-fobjc-arc",
            "-O2",
            "-framework",
            "Foundation",
            "-framework",
            "CoreGraphics",
            "-framework",
            "ImageIO",
            "-framework",
            "IOSurface",
            &source_path.to_string_lossy(),
            "-o",
            &temporary.to_string_lossy(),
        ]),
        cwd,
        METADATA_TIMEOUT,
    )?;
    fs::rename(&temporary, build.join("bridge")).map_err(|e| e.to_string())?;
    Ok(build.join("bridge"))
}

fn cache_lock(path: &Path, runner: &Runner, timeout: Duration) -> Result<fs::File, String> {
    use fs2::FileExt;
    use std::os::unix::fs::OpenOptionsExt;
    let file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| error.to_string())?;
    let deadline = Instant::now() + timeout;
    loop {
        runner.check_cancelled()?;
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(file),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(format!("Cannot lock the iOS preview build cache: {error}")),
        }
        if Instant::now() > deadline {
            return Err(
                "Another preview's Xcode build did not finish before the startup timeout.".into(),
            );
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn cleanup_command(mut command: Command, timeout: Duration) -> Result<(), String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    let child = command.spawn().map_err(|e| e.to_string())?;
    let group = child.id();
    let mut process = OwnedProcess {
        child,
        group,
        reaped: false,
    };
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if process.exited().is_some() {
            // Keep the group reserved until any descendants are stopped.
            unsafe {
                libc::kill(-(process.group as libc::pid_t), libc::SIGKILL);
            }
            let status = process.child.wait().map_err(|error| error.to_string())?;
            process.reaped = true;
            return if status.success() {
                Ok(())
            } else {
                Err(format!("Simulator cleanup failed: {status}"))
            };
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err("Simulator cleanup timed out.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn inventory() -> Value {
        json!({"runtimes":[{"identifier":"com.apple.CoreSimulator.SimRuntime.iOS-26-0", "name":"iOS 26.0", "version":"26.0", "isAvailable":true}, {"identifier":"com.apple.CoreSimulator.SimRuntime.iOS-99-0", "name":"iOS 99.0", "version":"99.0", "isAvailable":false}], "devicetypes":[{"name":"iPhone 17", "identifier":"phone17", "minRuntimeVersion":1703936}, {"name":"iPhone 16", "identifier":"phone16", "minRuntimeVersion":1179648}, {"name":"iPad Pro", "identifier":"ipad"}]})
    }
    #[test]
    fn installed_runtime_and_compatible_device_are_selected_together() {
        let selected = select_simulator(&inventory(), &IosPreviewSpec::default()).unwrap();
        assert_eq!(selected.device_type, "phone17");
        assert_eq!(selected.runtime_name, "iOS 26.0");
        let mut spec = IosPreviewSpec::default();
        spec.device_type = Some("iPhone 16".into());
        spec.runtime = Some("26.0".into());
        assert_eq!(
            select_simulator(&inventory(), &spec).unwrap().device_type,
            "phone16"
        );
        spec.runtime = Some("iOS 99.0".into());
        assert!(select_simulator(&inventory(), &spec).is_err());
        spec.runtime = None;
        spec.device_type = Some(uuid::Uuid::new_v4().to_string());
        assert!(
            select_simulator(&inventory(), &spec).is_err(),
            "A user's device UUID cannot be used as a device type"
        );
    }
    #[test]
    fn multiple_application_products_require_an_explicit_bundle() {
        let app = |id: &str| json!({"buildSettings":{"PRODUCT_TYPE":"com.apple.product-type.application", "PRODUCT_BUNDLE_IDENTIFIER":id,"TARGET_BUILD_DIR":"/tmp/build with space","FULL_PRODUCT_NAME":"Test.app"}});
        let settings = json!([app("one"), app("two"), {"buildSettings":{"PRODUCT_TYPE":"com.apple.product-type.framework"}}]);
        assert!(application_product(&settings, None).is_err());
        assert_eq!(
            application_product(&settings, Some("two")).unwrap(),
            (
                PathBuf::from("/tmp/build with space/Test.app"),
                "two".into()
            )
        );
    }

    #[test]
    fn crash_recovery_requires_a_dead_owner_and_exact_recorded_device_name() {
        let udid = uuid::Uuid::new_v4().to_string();
        let mut lease = Lease {
            owner_pid: i32::MAX as u32,
            udid: udid.clone(),
            device_name: "RiWork Preview isolated-lease".into(),
        };
        let inventory =
            json!({"devices":{"runtime":[{"udid":udid,"name":"RiWork Preview isolated-lease"}]}});
        assert!(stale_lease_matches(&lease, &inventory));
        lease.owner_pid = std::process::id();
        assert!(
            !stale_lease_matches(&lease, &inventory),
            "Another running RiWork's simulator stays untouched"
        );
        lease.owner_pid = i32::MAX as u32;
        lease.device_name = "RiWork Preview different-launch".into();
        assert!(
            !stale_lease_matches(&lease, &inventory),
            "Matching a prefix never grants cleanup ownership"
        );
    }
    #[test]
    fn cancelling_startup_terminates_only_its_running_command() {
        let state = Arc::new(Mutex::new(PreviewState::new(
            super::super::PreviewMode::IosSimulator,
        )));
        let cancellation = Arc::new(AtomicU64::new(0));
        let runner = Runner {
            state,
            cancellation: Arc::clone(&cancellation),
            revision: 0,
        };
        let worker = thread::spawn(move || {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", "sleep 60"]);
            runner.run(command, Duration::from_secs(10), true)
        });
        thread::sleep(Duration::from_millis(150));
        cancellation.fetch_add(1, Ordering::SeqCst);
        let start = Instant::now();
        assert!(worker.join().unwrap().unwrap_err().contains("cancelled"));
        assert!(start.elapsed() < Duration::from_secs(3));
    }
}
