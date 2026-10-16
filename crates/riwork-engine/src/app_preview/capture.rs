//! Native-window fallback. Every desktop call crosses RiWork's Cua MCP guard.

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    process::{ChildStdin, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

use super::cooperative::checked_png;
use super::{
    OwnedProcess, PreviewFrame, PreviewInput, PreviewPhase, PreviewState, PreviewWindow,
    set_message,
};

const MCP_LIMIT: usize = 16 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const CAPTURE_INTERVAL: Duration = Duration::from_millis(125);
const RETRY_INTERVAL: Duration = Duration::from_secs(3);

trait Driver {
    fn call(&mut self, name: &str, arguments: Value) -> Result<Value, String>;
    fn supports(&self, name: &str, property: &str) -> bool;
}

struct Binding {
    pid: u32,
    window_id: u64,
    original: Bounds,
    offscreen: bool,
}

#[derive(Clone, Copy)]
struct Bounds {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

pub(super) struct Capture {
    driver: Option<Box<dyn Driver + Send>>,
    binding: Option<Binding>,
    last_capture: Instant,
    retry_after: Instant,
    sequence: u64,
    ever_bound: bool,
}

impl Capture {
    pub fn new() -> Self {
        Self {
            driver: None,
            binding: None,
            last_capture: Instant::now() - CAPTURE_INTERVAL,
            retry_after: Instant::now(),
            sequence: 0,
            ever_bound: false,
        }
    }

    pub fn tick(
        &mut self,
        process: &mut OwnedProcess,
        visible: bool,
        state: &Arc<Mutex<PreviewState>>,
    ) -> Result<(), String> {
        if !visible {
            set_message(
                state,
                PreviewPhase::Paused,
                "Capture paused while this tab is hidden.",
            );
            return Ok(());
        }
        if Instant::now() < self.retry_after {
            return Ok(());
        }
        let result = self.tick_inner(process, state);
        if result.is_err() {
            self.retry_after = Instant::now() + RETRY_INTERVAL;
        }
        result
    }

    fn tick_inner(
        &mut self,
        process: &mut OwnedProcess,
        state: &Arc<Mutex<PreviewState>>,
    ) -> Result<(), String> {
        if self.driver.is_none() {
            self.driver = Some(Box::new(McpDriver::connect().map_err(setup_error)?));
        }
        if self.binding.is_none() {
            let windows = self.find_windows(process)?;
            state.lock().unwrap_or_else(|e| e.into_inner()).windows =
                windows.iter().map(|(window, _)| window.clone()).collect();
            if windows.len() == 1 && !self.ever_bound {
                let (window, bounds) = &windows[0];
                self.bind(window.pid, window.window_id, *bounds, process, state)?;
            } else {
                set_message(
                    state,
                    PreviewPhase::WaitingForWindow,
                    if self.ever_bound {
                        "The bound window closed. Select a window to continue; RiWork will not redirect input automatically.".into()
                    } else if windows.len() > 1 {
                        format!(
                            "{} launched windows found. Select the window to preview.",
                            windows.len()
                        )
                    } else {
                        "Waiting for a window in the launched process group…".into()
                    },
                );
                self.last_capture = Instant::now();
                self.retry_after = Instant::now() + Duration::from_millis(500);
                return Ok(());
            }
        }
        if self.last_capture.elapsed() < CAPTURE_INTERVAL {
            return Ok(());
        }
        self.observe(process, state, true)?;
        self.last_capture = Instant::now();
        Ok(())
    }

    fn find_windows(
        &mut self,
        process: &mut OwnedProcess,
    ) -> Result<Vec<(PreviewWindow, Bounds)>, String> {
        let driver = self.driver.as_mut().ok_or("Cua Driver is not connected.")?;
        // Learn desktop app pids through Cua; kernel group ownership is an
        // additional check, never a title/name-based attribution heuristic.
        let apps = tool_data(&driver.call("list_apps", json!({}))?);
        let mut pids = vec![process.group];
        if let Some(apps) = apps.get("apps").and_then(Value::as_array) {
            for app in apps {
                if let Some(pid) = app
                    .get("pid")
                    .and_then(Value::as_u64)
                    .and_then(|pid| u32::try_from(pid).ok())
                {
                    if !pids.contains(&pid) && process.owns(pid) {
                        pids.push(pid);
                    }
                }
            }
        }
        let mut windows = Vec::new();
        for pid in pids {
            if !process.owns(pid) {
                continue;
            }
            let listed = tool_data(&driver.call("list_windows", json!({"pid":pid}))?);
            if let Some(listed) = listed.get("windows").and_then(Value::as_array) {
                for window in listed {
                    if window.get("pid").and_then(Value::as_u64) != Some(pid as u64) {
                        continue;
                    }
                    if window
                        .get("layer")
                        .and_then(Value::as_i64)
                        .is_some_and(|layer| layer != 0)
                    {
                        continue;
                    }
                    let Some(window_id) = window.get("window_id").and_then(Value::as_u64) else {
                        continue;
                    };
                    let Some(bounds) = read_bounds(window.get("bounds")) else {
                        continue;
                    };
                    if bounds.width < 80.0 || bounds.height < 60.0 {
                        continue;
                    }
                    windows.push((
                        PreviewWindow {
                            pid,
                            window_id,
                            title: window
                                .get("title")
                                .and_then(Value::as_str)
                                .unwrap_or("Untitled window")
                                .into(),
                        },
                        bounds,
                    ));
                }
            }
        }
        Ok(windows)
    }

    pub fn select_window(
        &mut self,
        pid: u32,
        window_id: u64,
        process: &mut OwnedProcess,
        state: &Arc<Mutex<PreviewState>>,
    ) -> Result<(), String> {
        let windows = self.find_windows(process)?;
        let (_, bounds) = windows
            .iter()
            .find(|(window, _)| window.pid == pid && window.window_id == window_id)
            .ok_or("The selected window no longer belongs to this preview launch.")?;
        self.bind(pid, window_id, *bounds, process, state)
    }

    fn bind(
        &mut self,
        pid: u32,
        window_id: u64,
        original: Bounds,
        process: &mut OwnedProcess,
        state: &Arc<Mutex<PreviewState>>,
    ) -> Result<(), String> {
        if !process.owns(pid) {
            return Err("Refused to bind a window outside this preview launch.".into());
        }
        self.binding = Some(Binding {
            pid,
            window_id,
            original,
            offscreen: false,
        });
        self.ever_bound = true;
        {
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            state.pid = Some(pid);
            state.window_id = Some(window_id);
        }
        // Capture once before moving: this proves the exact owner/window and
        // available backing store rather than blindly hiding an unseen target.
        self.observe(process, state, true)?;
        let driver = self.driver.as_mut().expect("connected");
        let moved = driver.call("set_window_frame", json!({"pid":pid,"window_id":window_id,
            "x":-10000.0-original.width,"y":original.y,"width":original.width,"height":original.height}));
        match moved {
            Ok(result) if effect_confirmed(&result) => {
                self.binding.as_mut().expect("bound").offscreen = true;
            }
            Ok(_) | Err(_) => {
                state.lock().unwrap_or_else(|e| e.into_inner()).warning =
                    Some("macOS did not confirm moving the app offscreen. Use windowless SDK mode for a fully hidden app.".into());
            }
        }
        Ok(())
    }

    fn observe(
        &mut self,
        process: &mut OwnedProcess,
        state: &Arc<Mutex<PreviewState>>,
        publish: bool,
    ) -> Result<Value, String> {
        let binding = self
            .binding
            .as_ref()
            .ok_or("No preview window is selected.")?;
        if !process.owns(binding.pid) {
            return Err(
                "The bound app no longer belongs to this launch; input is disabled.".into(),
            );
        }
        let driver = self.driver.as_mut().expect("connected");
        let mut args = json!({"pid":binding.pid,"window_id":binding.window_id});
        if driver.supports("get_window_state", "include_accessibility_tree") {
            args["include_accessibility_tree"] = json!(false);
        }
        if driver.supports("get_window_state", "max_dimension") {
            args["max_dimension"] = json!(1600);
        }
        let result = match driver.call("get_window_state", args) {
            Ok(result) => result,
            Err(error) => {
                if error.contains("window_id_not_found")
                    || error.contains("window_owner_pid_mismatch")
                {
                    self.binding = None;
                    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                    state.window_id = None;
                    state.frame = None;
                }
                return Err(setup_error(error));
            }
        };
        let data = tool_data(&result);
        if data
            .get("pid")
            .and_then(Value::as_u64)
            .is_some_and(|pid| pid != binding.pid as u64)
            || data
                .get("window_id")
                .and_then(Value::as_u64)
                .is_some_and(|id| id != binding.window_id)
        {
            return Err("Cua returned a different app/window than the bound preview.".into());
        }
        let (encoded, width, height) = screenshot(&result, &data)?;
        let png = checked_png(encoded, width, height)?;
        if publish {
            self.sequence = self.sequence.wrapping_add(1);
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            state.frame = Some(Arc::new(PreviewFrame {
                png,
                width,
                height,
                sequence: self.sequence,
            }));
            state.phase = PreviewPhase::Streaming;
            state.message = if binding.offscreen {
                "Cua window capture · window moved offscreen · background input may be unavailable"
                    .into()
            } else {
                "Cua window capture · native desktop window still exists".into()
            };
        }
        Ok(result)
    }

    pub fn input(
        &mut self,
        input: PreviewInput,
        process: &mut OwnedProcess,
        state: &Arc<Mutex<PreviewState>>,
    ) -> Result<(), String> {
        if matches!(input, PreviewInput::Refresh) {
            self.observe(process, state, true)?;
            return Ok(());
        }
        if matches!(
            input,
            PreviewInput::Focus { .. }
                | PreviewInput::KeyUp { .. }
                | PreviewInput::PointerMove { .. }
        ) {
            return Ok(());
        }
        if matches!(input, PreviewInput::ShowWindow) {
            return self.show_window(process);
        }
        let frame = state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .frame
            .clone()
            .ok_or("Wait for a preview frame before sending input.")?;
        // Fresh window state is required before every input transaction. The
        // observed capture and the frame the user clicked must have the same size.
        let observation = self.observe(process, state, false)?;
        let data = tool_data(&observation);
        let (_, width, height) = screenshot(&observation, &data)?;
        if (width, height) != (frame.width, frame.height) {
            return Err("The app resized; wait for the new preview frame and try again.".into());
        }
        let binding = self
            .binding
            .as_mut()
            .ok_or("No preview window is selected.")?;
        let pid = binding.pid;
        let window_id = binding.window_id;
        let driver = self.driver.as_mut().expect("connected");
        let mut args = json!({"pid":pid,"window_id":window_id});
        let (tool, pointer) = match input {
            PreviewInput::Click { x, y, button } => {
                check_point(x, y, width, height)?;
                args["x"] = json!(x);
                args["y"] = json!(y);
                args["button"] = json!(button);
                ("click", true)
            }
            PreviewInput::Text { text } => {
                args["text"] = json!(text);
                ("type_text", false)
            }
            PreviewInput::Key { key } => {
                let (key, modifiers) = driver_key(&key);
                args["key"] = json!(key);
                if !modifiers.is_empty() {
                    args["modifiers"] = json!(modifiers);
                }
                ("press_key", false)
            }
            PreviewInput::Scroll {
                x,
                y,
                delta_x,
                delta_y,
            } => {
                check_point(x, y, width, height)?;
                if !delta_x.is_finite() || !delta_y.is_finite() {
                    return Err("Invalid preview scroll delta.".into());
                }
                args["x"] = json!(x);
                args["y"] = json!(y);
                let vertical = delta_y.abs() >= delta_x.abs();
                let delta = if vertical { delta_y } else { delta_x };
                args["direction"] = json!(if vertical {
                    if delta > 0.0 { "up" } else { "down" }
                } else if delta > 0.0 {
                    "left"
                } else {
                    "right"
                });
                args["amount"] = json!((delta.abs() / 24.0).round().clamp(1.0, 10.0));
                ("scroll", true)
            }
            PreviewInput::Resize { width, height } => {
                let width = width.clamp(160, 2048);
                let height = height.clamp(120, 2048);
                args["x"] = json!(if binding.offscreen {
                    -10000.0 - width as f64
                } else {
                    binding.original.x
                });
                args["y"] = json!(binding.original.y);
                args["width"] = json!(width);
                args["height"] = json!(height);
                ("set_window_frame", false)
            }
            PreviewInput::ShowWindow | PreviewInput::Refresh => {
                unreachable!("handled before screenshot requirement")
            }
            PreviewInput::Focus { .. }
            | PreviewInput::KeyUp { .. }
            | PreviewInput::PointerMove { .. } => return Ok(()),
            PreviewInput::PointerDown { .. } | PreviewInput::PointerUp { .. } => {
                return Err(
                    "Native capture supports clicks; dragging needs the windowless preview SDK."
                        .into(),
                );
            }
        };
        if pointer {
            if let Some(capture_id) = data
                .get("capture_id")
                .filter(|_| driver.supports(tool, "capture_id"))
            {
                args["capture_id"] = capture_id.clone();
            }
        }
        if driver.supports(tool, "delivery_mode") {
            args["delivery_mode"] = json!("background");
        }
        let result = driver.call(tool, args)?;
        let data = tool_data(&result);
        let effect = data
            .get("effect")
            .and_then(Value::as_str)
            .unwrap_or("unverifiable");
        if effect != "confirmed" {
            state.lock().unwrap_or_else(|e| e.into_inner()).warning = Some(format!(
                "Cua input {effect}; inspect the preview. Offscreen apps may require the windowless SDK for reliable input."
            ));
        }
        self.last_capture = Instant::now() - CAPTURE_INTERVAL;
        Ok(())
    }

    fn show_window(&mut self, process: &mut OwnedProcess) -> Result<(), String> {
        // Recovery must remain available if moving offscreen stopped capture.
        // Geometry is grounded through list_windows, without needing a frame.
        let binding = self
            .binding
            .as_mut()
            .ok_or("No preview window is selected.")?;
        if !process.owns(binding.pid) {
            return Err("The bound window is outside this preview launch.".into());
        }
        let driver = self.driver.as_mut().ok_or("Cua Driver is not connected.")?;
        let windows = tool_data(&driver.call("list_windows", json!({"pid":binding.pid}))?);
        let found = windows
            .get("windows")
            .and_then(Value::as_array)
            .is_some_and(|windows| {
                windows.iter().any(|window| {
                    window.get("pid").and_then(Value::as_u64) == Some(binding.pid as u64)
                        && window.get("window_id").and_then(Value::as_u64)
                            == Some(binding.window_id)
                })
            });
        if !found {
            return Err("The bound preview window has closed.".into());
        }
        let result = driver.call("set_window_frame", json!({"pid":binding.pid,"window_id":binding.window_id,
            "x":binding.original.x,"y":binding.original.y,"width":binding.original.width,"height":binding.original.height}))?;
        if !effect_confirmed(&result) {
            return Err("Cua could not confirm restoring the window's desktop position.".into());
        }
        binding.offscreen = false;
        self.last_capture = Instant::now() - CAPTURE_INTERVAL;
        Ok(())
    }
}

fn check_point(x: f32, y: f32, width: u32, height: u32) -> Result<(), String> {
    if x.is_finite()
        && y.is_finite()
        && x >= 0.0
        && y >= 0.0
        && x < width as f32
        && y < height as f32
    {
        Ok(())
    } else {
        Err("Preview pointer is outside the current captured frame.".into())
    }
}

fn driver_key(key: &str) -> (String, Vec<&str>) {
    let mut parts: Vec<_> = key.split('-').collect();
    let key = parts.pop().unwrap_or(key);
    let key = match key {
        "enter" => "return",
        "backspace" => "delete",
        "page_down" => "pagedown",
        "page_up" => "pageup",
        other => other,
    };
    (
        key.into(),
        parts
            .into_iter()
            .filter(|part| ["cmd", "ctrl", "shift", "alt", "option", "fn"].contains(part))
            .collect(),
    )
}

fn read_bounds(value: Option<&Value>) -> Option<Bounds> {
    let value = value?;
    Some(Bounds {
        x: value.get("x")?.as_f64()?,
        y: value.get("y")?.as_f64()?,
        width: value.get("width")?.as_f64()?,
        height: value.get("height")?.as_f64()?,
    })
}

fn setup_error(error: String) -> String {
    format!("{error} Open RiWork Settings → Computer Use to check Cua setup and macOS permissions.")
}

fn effect_confirmed(result: &Value) -> bool {
    tool_data(result).get("effect").and_then(Value::as_str) == Some("confirmed")
}

fn tool_data(result: &Value) -> Value {
    if let Some(data) = result.get("structuredContent") {
        return data.clone();
    }
    if let Some(content) = result.get("content").and_then(Value::as_array) {
        for block in content {
            if let Some(text) = block.get("text").and_then(Value::as_str) {
                if let Ok(value) = serde_json::from_str(text) {
                    return value;
                }
            }
        }
    }
    result.clone()
}

fn screenshot<'a>(result: &'a Value, data: &Value) -> Result<(&'a str, u32, u32), String> {
    let image = result.get("content").and_then(Value::as_array).and_then(|blocks| blocks.iter().find(|block| block.get("type").and_then(Value::as_str) == Some("image")))
        .and_then(|block| block.get("data")).and_then(Value::as_str).ok_or("Cua did not return a screenshot. Check Cua setup and Screen Recording permission; offscreen or minimized apps may stop rendering.")?;
    let width = data
        .get("screenshot_width")
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .ok_or("Cua screenshot width is missing.")?;
    let height = data
        .get("screenshot_height")
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .ok_or("Cua screenshot height is missing.")?;
    Ok((image, width, height))
}

struct McpDriver {
    process: OwnedProcess,
    stdin: Option<ChildStdin>,
    lines: mpsc::Receiver<Result<Value, String>>,
    tools: HashMap<String, Value>,
    request_id: u64,
    stderr: Arc<Mutex<String>>,
}

impl McpDriver {
    fn connect() -> Result<Self, String> {
        // Desktop's entrypoint dispatches CLI commands before starting GPUI.
        // The existing server verifies the driver signature and protects all
        // live RiWork windows. No direct desktop CLI commands are executed.
        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        let mut command = Command::new(executable);
        command
            .args(["cua", "mcp"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env_remove(crate::cua::ALLOW_RIWORK_INPUT);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("Cannot start RiWork Cua MCP server: {error}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or("Cannot write to Cua MCP server.")?;
        let stdout = child.stdout.take().ok_or("Cannot read Cua MCP server.")?;
        let errors = child
            .stderr
            .take()
            .ok_or("Cannot read Cua MCP server errors.")?;
        let (sender, lines) = mpsc::sync_channel(4);
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let line = read_line_bounded(&mut reader, MCP_LIMIT).and_then(|line| {
                    if line.is_empty() {
                        return Err("Cua MCP server disconnected.".into());
                    }
                    serde_json::from_slice(&line)
                        .map_err(|error| format!("Invalid Cua MCP response: {error}"))
                });
                let failed = line.is_err();
                if sender.send(line).is_err() || failed {
                    break;
                }
            }
        });
        let stderr = Arc::new(Mutex::new(String::new()));
        let collected = Arc::clone(&stderr);
        thread::spawn(move || {
            let mut errors = errors;
            let mut bytes = [0; 1024];
            while let Ok(count) = errors.read(&mut bytes) {
                if count == 0 {
                    break;
                }
                let mut text = collected.lock().unwrap_or_else(|e| e.into_inner());
                text.push_str(&String::from_utf8_lossy(&bytes[..count]));
                if text.len() > 16 * 1024 {
                    let keep = text.chars().rev().take(8192).collect::<String>();
                    *text = keep.chars().rev().collect();
                }
            }
        });
        let group = child.id();
        let mut driver = Self {
            process: OwnedProcess {
                child,
                group,
                reaped: false,
            },
            stdin: Some(stdin),
            lines,
            tools: HashMap::new(),
            request_id: 0,
            stderr,
        };
        driver.request("initialize", json!({"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"riwork-app-preview","version":"1"}}))?;
        driver.write(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))?;
        let listed = driver.request("tools/list", json!({}))?;
        let tools = listed
            .get("tools")
            .and_then(Value::as_array)
            .ok_or("Cua MCP did not advertise its tools.")?;
        for tool in tools {
            if let Some(name) = tool.get("name").and_then(Value::as_str) {
                driver.tools.insert(name.into(), tool.clone());
            }
        }
        for required in [
            "list_apps",
            "list_windows",
            "get_window_state",
            "set_window_frame",
        ] {
            if !driver.tools.contains_key(required) {
                return Err(format!(
                    "Cua Driver does not advertise required tool {required}; update it through RiWork Cua setup."
                ));
            }
        }
        Ok(driver)
    }

    fn write(&mut self, message: &Value) -> Result<(), String> {
        let mut bytes = serde_json::to_vec(message).map_err(|error| error.to_string())?;
        bytes.push(b'\n');
        let stdin = self.stdin.as_mut().ok_or("Cua MCP input is closed.")?;
        stdin
            .write_all(&bytes)
            .and_then(|_| stdin.flush())
            .map_err(|error| format!("Cannot write Cua MCP request: {error}"))
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.request_id += 1;
        let id = self.request_id;
        self.write(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))?;
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        loop {
            if Instant::now() >= deadline {
                return Err(format!("Cua MCP request {method} timed out."));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let response = self.lines.recv_timeout(remaining).map_err(|error| {
                format!(
                    "Cua MCP request {method} failed: {error}. {}",
                    self.stderr.lock().unwrap_or_else(|e| e.into_inner()).trim()
                )
            })??;
            if response.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = response.get("error") {
                return Err(format!("Cua MCP {method}: {error}"));
            }
            return response
                .get("result")
                .cloned()
                .ok_or("Cua MCP response has no result.".into());
        }
    }
}

impl Driver for McpDriver {
    fn call(&mut self, name: &str, arguments: Value) -> Result<Value, String> {
        if !self.tools.contains_key(name) {
            return Err(format!(
                "Cua Driver does not advertise {name}; use RiWork Cua setup to update the driver."
            ));
        }
        let result = self.request("tools/call", json!({"name":name,"arguments":arguments}))?;
        let data = tool_data(&result);
        if result.get("isError").and_then(Value::as_bool) == Some(true)
            || data.get("effect").and_then(Value::as_str) == Some("refused")
            || data.get("error").is_some_and(|error| !error.is_null())
        {
            return Err(format!("Cua Driver {name}: {data}"));
        }
        Ok(result)
    }

    fn supports(&self, name: &str, property: &str) -> bool {
        self.tools
            .get(name)
            .and_then(|tool| tool.pointer("/inputSchema/properties"))
            .and_then(|properties| properties.get(property))
            .is_some()
    }
}

impl Drop for McpDriver {
    fn drop(&mut self) {
        drop(self.stdin.take());
        // The proxy closes its driver child's stdin and gives it 500 ms to
        // exit. Let that cleanup finish before terminating a stuck proxy.
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if self.process.exited().is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        self.process.stop();
    }
}

fn read_line_bounded(reader: &mut impl BufRead, limit: usize) -> Result<Vec<u8>, String> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().map_err(|error| error.to_string())?;
        if available.is_empty() {
            return Ok(line);
        }
        let length = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |end| end + 1);
        if line.len() + length > limit {
            return Err("Cua MCP response exceeded the 16 MiB limit.".into());
        }
        let ended = available[length - 1] == b'\n';
        line.extend_from_slice(&available[..length]);
        reader.consume(length);
        if ended {
            return Ok(line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn mcp_reader_bounds_unterminated_output() {
        assert!(read_line_bounded(&mut Cursor::new(vec![b'x'; 17]), 16).is_err());
        assert_eq!(
            read_line_bounded(&mut Cursor::new(b"ok\nnext"), 16).unwrap(),
            b"ok\n"
        );
    }

    #[test]
    fn data_is_read_from_mcp_structured_output_or_text_without_confusing_images() {
        assert_eq!(
            tool_data(
                &json!({"content":[{"type":"image","data":"binary"},{"type":"text","text":"{\"pid\":7}"}]})
            )["pid"],
            7
        );
        assert_eq!(
            tool_data(&json!({"structuredContent":{"pid":8},"content":[]}))["pid"],
            8
        );
    }

    #[test]
    fn point_and_key_mapping_preserve_frame_scope() {
        assert!(check_point(20.0, 30.0, 100, 100).is_ok());
        assert!(check_point(f32::NAN, 0.0, 100, 100).is_err());
        assert!(check_point(100.0, 0.0, 100, 100).is_err());
        assert_eq!(
            driver_key("cmd-shift-enter"),
            ("return".into(), vec!["cmd", "shift"])
        );
    }

    struct FakeDriver {
        pid: u32,
        wrong_owner: bool,
        calls: Arc<Mutex<Vec<(String, Value)>>>,
        png: String,
    }

    impl Driver for FakeDriver {
        fn call(&mut self, name: &str, arguments: Value) -> Result<Value, String> {
            self.calls.lock().unwrap().push((name.into(), arguments));
            Ok(match name {
                "list_apps" => {
                    json!({"structuredContent":{"apps":[{"pid":self.pid},{"pid":std::process::id()}]}})
                }
                "list_windows" => {
                    json!({"structuredContent":{"windows":[{"pid":self.pid,"window_id":77,"title":"fixture","layer":0,"bounds":{"x":30,"y":40,"width":100,"height":100}}]}})
                }
                "get_window_state" => {
                    json!({"structuredContent":{"pid":if self.wrong_owner {std::process::id()} else {self.pid},"window_id":77,"screenshot_width":2,"screenshot_height":3,"capture_id":"frame-1"},"content":[{"type":"image","data":self.png,"mimeType":"image/png"}]})
                }
                _ => json!({"structuredContent":{"effect":"unverifiable"}}),
            })
        }
        fn supports(&self, _name: &str, _property: &str) -> bool {
            true
        }
    }

    fn fixture(
        wrong_owner: bool,
    ) -> (
        OwnedProcess,
        Capture,
        Arc<Mutex<PreviewState>>,
        Arc<Mutex<Vec<(String, Value)>>>,
    ) {
        use base64::Engine;
        use std::os::unix::process::CommandExt;
        let child = Command::new("/bin/sleep")
            .arg("60")
            .process_group(0)
            .spawn()
            .unwrap();
        let group = child.id();
        let process = OwnedProcess {
            child,
            group,
            reaped: false,
        };
        let mut bytes = Cursor::new(Vec::new());
        image::RgbaImage::new(2, 3)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        let png = base64::engine::general_purpose::STANDARD.encode(bytes.into_inner());
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut capture = Capture::new();
        capture.driver = Some(Box::new(FakeDriver {
            pid: group,
            wrong_owner,
            calls: Arc::clone(&calls),
            png,
        }));
        capture.binding = Some(Binding {
            pid: group,
            window_id: 77,
            original: Bounds {
                x: 30.0,
                y: 40.0,
                width: 100.0,
                height: 100.0,
            },
            offscreen: true,
        });
        capture.ever_bound = true;
        let state = Arc::new(Mutex::new(PreviewState::new(
            super::super::PreviewMode::WindowCapture,
        )));
        state.lock().unwrap().frame = Some(Arc::new(PreviewFrame {
            png: Vec::new(),
            width: 2,
            height: 3,
            sequence: 1,
        }));
        (process, capture, state, calls)
    }

    #[test]
    fn captured_input_is_preceded_by_exact_window_observation_and_stays_in_background() {
        let (mut process, mut capture, state, calls) = fixture(false);
        capture
            .input(
                PreviewInput::Click {
                    x: 1.0,
                    y: 1.0,
                    button: super::super::PreviewMouseButton::Left,
                },
                &mut process,
                &state,
            )
            .unwrap();
        let calls = calls.lock().unwrap();
        assert_eq!(
            calls
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            vec!["get_window_state", "click"]
        );
        for (_, args) in calls.iter() {
            assert_eq!(args["pid"], process.group);
            assert_eq!(args["window_id"], 77);
            assert_ne!(args["scope"], "desktop");
            assert_ne!(args["delivery_mode"], "foreground");
        }
        assert_eq!(calls[1].1["capture_id"], "frame-1");
        assert_eq!(calls[1].1["delivery_mode"], "background");
        assert!(
            state
                .lock()
                .unwrap()
                .warning
                .as_deref()
                .unwrap()
                .contains("unverifiable")
        );
    }

    #[test]
    fn owner_mismatch_refuses_input_without_sending_the_action() {
        let (mut process, mut capture, state, calls) = fixture(true);
        assert!(
            capture
                .input(
                    PreviewInput::Text {
                        text: "should not type".into()
                    },
                    &mut process,
                    &state
                )
                .is_err()
        );
        assert_eq!(calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn closed_window_is_never_silently_replaced_by_a_new_owned_window() {
        let (mut process, mut capture, state, calls) = fixture(false);
        capture.binding = None;
        capture.tick(&mut process, true, &state).unwrap();
        assert!(capture.binding.is_none());
        assert_eq!(state.lock().unwrap().windows.len(), 1);
        assert_eq!(state.lock().unwrap().phase, PreviewPhase::WaitingForWindow);
        assert!(
            calls
                .lock()
                .unwrap()
                .iter()
                .all(|(name, _)| name == "list_apps" || name == "list_windows")
        );
    }
}
