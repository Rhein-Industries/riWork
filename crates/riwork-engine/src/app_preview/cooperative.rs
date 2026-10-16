//! Private, authenticated IPC for apps that render without a desktop window.

use std::{
    fs,
    io::{self, Cursor, Read, Write},
    os::unix::{
        fs::{DirBuilderExt, PermissionsExt},
        io::AsRawFd,
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use riwork_core::app_preview::{
    MAX_PREVIEW_DIMENSION, MAX_PREVIEW_INPUT_MESSAGE_BYTES, MAX_PREVIEW_MESSAGE_BYTES,
    PREVIEW_PROTOCOL_VERSION, PreviewClientMessage, PreviewHostMessage,
};

use super::{OwnedProcess, PreviewFrame, PreviewInput, PreviewPhase, PreviewState, set_message};

pub(super) struct Endpoint {
    pub path: PathBuf,
    pub token: String,
    directory: PathBuf,
    listener: UnixListener,
    peer: Option<Peer>,
    sequence: u64,
    visible: bool,
}

struct Peer {
    stream: UnixStream,
    peer_pid: u32,
    authenticated: bool,
    connected: Instant,
    input: Vec<u8>,
    output: Vec<u8>,
}

impl Endpoint {
    pub fn open() -> Result<Self, String> {
        // /tmp keeps AF_UNIX paths below macOS's small sockaddr_un limit even
        // when TMPDIR points deep into a per-user application container.
        let directory =
            PathBuf::from("/tmp").join(format!("riwork-preview-{}", uuid::Uuid::new_v4().simple()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .map_err(|error| format!("Cannot create private preview socket folder: {error}"))?;
        let path = directory.join("preview.sock");
        let listener = match UnixListener::bind(&path) {
            Ok(listener) => listener,
            Err(error) => {
                let _ = fs::remove_dir(&directory);
                return Err(format!("Cannot bind preview socket: {error}"));
            }
        };
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("Cannot protect preview socket: {error}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        Ok(Self {
            path,
            token: uuid::Uuid::new_v4().to_string(),
            directory,
            listener,
            peer: None,
            sequence: 0,
            visible: true,
        })
    }

    pub fn tick(
        &mut self,
        process: &mut OwnedProcess,
        visible: bool,
        state: &Arc<Mutex<PreviewState>>,
    ) -> Result<(), String> {
        if self.peer.is_none() {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    let pid = peer_pid(&stream)?;
                    if !process.owns(pid) {
                        return Err(
                            "Rejected preview connection from a process outside this launch."
                                .into(),
                        );
                    }
                    stream
                        .set_nonblocking(true)
                        .map_err(|error| error.to_string())?;
                    self.peer = Some(Peer {
                        stream,
                        peer_pid: pid,
                        authenticated: false,
                        connected: Instant::now(),
                        input: Vec::new(),
                        output: Vec::new(),
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) => return Err(format!("Cannot accept app preview: {error}")),
            }
        }
        let result = self.tick_peer(process, visible, state);
        if result.is_err() {
            self.peer = None;
        }
        result
    }

    fn tick_peer(
        &mut self,
        process: &mut OwnedProcess,
        visible: bool,
        state: &Arc<Mutex<PreviewState>>,
    ) -> Result<(), String> {
        let peer = self.peer.as_mut().expect("accepted peer");
        if !process.owns(peer.peer_pid) {
            return Err("Preview app disconnected from its launched process group.".into());
        }
        if !peer.authenticated && peer.connected.elapsed() > Duration::from_secs(3) {
            return Err("App preview handshake timed out.".into());
        }
        peer.read()?;
        // Work is bounded per tick; frames are replaced rather than queued in UI state.
        for _ in 0..8 {
            let Some(end) = peer.input.iter().position(|byte| *byte == b'\n') else {
                break;
            };
            let line: Vec<u8> = peer.input.drain(..=end).collect();
            let message: PreviewClientMessage = serde_json::from_slice(&line)
                .map_err(|error| format!("Invalid app preview protocol: {error}"))?;
            if !peer.authenticated {
                match message {
                    PreviewClientMessage::Hello {
                        token,
                        pid,
                        protocol,
                    } if token == self.token
                        && pid == peer.peer_pid
                        && protocol == PREVIEW_PROTOCOL_VERSION =>
                    {
                        peer.authenticated = true;
                        peer.send(&PreviewHostMessage::Ready {
                            protocol: PREVIEW_PROTOCOL_VERSION,
                        })?;
                        peer.send(&PreviewHostMessage::Visibility { visible })?;
                        self.visible = visible;
                        state.lock().unwrap_or_else(|e| e.into_inner()).pid = Some(pid);
                        set_message(
                            state,
                            PreviewPhase::WaitingForWindow,
                            "Connected to windowless app; waiting for its first frame…",
                        );
                    }
                    _ => {
                        return Err(
                            "App preview authentication or protocol version did not match.".into(),
                        );
                    }
                }
                continue;
            }
            match message {
                PreviewClientMessage::Frame { png, width, height } => {
                    let png = checked_png(&png, width, height)?;
                    if visible {
                        self.sequence = self.sequence.wrapping_add(1);
                        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                        state.frame = Some(Arc::new(PreviewFrame {
                            png,
                            width,
                            height,
                            sequence: self.sequence,
                        }));
                        state.phase = PreviewPhase::Streaming;
                        state.message = if state.mode == super::PreviewMode::IosSimulator {
                            "iOS simulator · live stream and touch input".into()
                        } else {
                            "Windowless app preview · separate process".into()
                        };
                    }
                }
                PreviewClientMessage::Error { message } => {
                    return Err(format!(
                        "App preview: {}",
                        message.chars().take(2048).collect::<String>()
                    ));
                }
                PreviewClientMessage::Hello { .. } => {
                    return Err("App sent a duplicate preview handshake.".into());
                }
            }
        }
        if self.visible != visible {
            peer.send(&PreviewHostMessage::Visibility { visible })?;
            self.visible = visible;
        }
        if !visible {
            set_message(
                state,
                PreviewPhase::Paused,
                "Preview paused while this tab is hidden.",
            );
        } else if state.lock().unwrap_or_else(|e| e.into_inner()).phase == PreviewPhase::Paused {
            set_message(
                state,
                PreviewPhase::WaitingForWindow,
                "Resuming windowless app preview…",
            );
        }
        peer.flush()
    }

    pub fn input(&mut self, input: PreviewInput) -> Result<(), String> {
        let peer = self
            .peer
            .as_mut()
            .filter(|peer| peer.authenticated)
            .ok_or("Waiting for the app's preview SDK connection.")?;
        if matches!(input, PreviewInput::ShowWindow) {
            return Err("This app is windowless; its interface is in this tab.".into());
        }
        peer.send(&PreviewHostMessage::Input { input })?;
        peer.flush()
    }

    pub fn shutdown(&mut self) {
        if let Some(peer) = &mut self.peer {
            let _ = peer.send(&PreviewHostMessage::Shutdown);
            let _ = peer.flush();
        }
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        self.shutdown();
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_dir(&self.directory);
    }
}

impl Peer {
    fn read(&mut self) -> Result<(), String> {
        if self.input.contains(&b'\n') {
            return Ok(());
        }
        let mut bytes = [0; 64 * 1024];
        for _ in 0..64 {
            let capacity = MAX_PREVIEW_MESSAGE_BYTES
                .saturating_sub(self.input.len())
                .min(bytes.len());
            if capacity == 0 {
                return Err("App preview frame exceeded the 16 MiB protocol limit.".into());
            }
            match self.stream.read(&mut bytes[..capacity]) {
                Ok(0) => return Err("The app closed its preview connection.".into()),
                Ok(count) => {
                    let complete_line = bytes[..count].contains(&b'\n');
                    self.input.extend_from_slice(&bytes[..count]);
                    if complete_line {
                        break;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(format!("Cannot read app preview: {error}")),
            }
        }
        Ok(())
    }

    fn send(&mut self, message: &PreviewHostMessage) -> Result<(), String> {
        let mut bytes = serde_json::to_vec(message).map_err(|error| error.to_string())?;
        bytes.push(b'\n');
        if bytes.len() > MAX_PREVIEW_INPUT_MESSAGE_BYTES {
            return Err("Encoded preview input exceeds the 64 KiB message limit.".into());
        }
        if self.output.len() + bytes.len() > 256 * 1024 {
            return Err("App is not reading preview input; pending input was refused.".into());
        }
        self.output.extend(bytes);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), String> {
        while !self.output.is_empty() {
            match self.stream.write(&self.output) {
                Ok(0) => return Err("App preview input connection closed.".into()),
                Ok(count) => {
                    self.output.drain(..count);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(format!("Cannot send app preview input: {error}")),
            }
        }
        Ok(())
    }
}

pub(super) fn checked_png(encoded: &str, width: u32, height: u32) -> Result<Vec<u8>, String> {
    if width == 0 || height == 0 || width > MAX_PREVIEW_DIMENSION || height > MAX_PREVIEW_DIMENSION
    {
        return Err("Preview dimensions must be between 1 and 2048 pixels.".into());
    }
    if encoded.len() > MAX_PREVIEW_MESSAGE_BYTES {
        return Err("Preview PNG exceeds the protocol limit.".into());
    }
    let png = STANDARD
        .decode(encoded)
        .map_err(|error| format!("Invalid preview PNG encoding: {error}"))?;
    let dimensions = image::ImageReader::with_format(Cursor::new(&png), image::ImageFormat::Png)
        .into_dimensions()
        .map_err(|error| format!("Invalid preview PNG: {error}"))?;
    if dimensions != (width, height) {
        return Err("Preview PNG dimensions do not match the frame header.".into());
    }
    Ok(png)
}

fn peer_pid(stream: &UnixStream) -> Result<u32, String> {
    #[cfg(target_os = "macos")]
    {
        let mut pid: libc::pid_t = 0;
        let mut length = std::mem::size_of_val(&pid) as libc::socklen_t;
        // SAFETY: getsockopt writes a pid-sized LOCAL_PEERPID result to the buffer.
        let result = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_LOCAL,
                libc::LOCAL_PEERPID,
                (&mut pid as *mut libc::pid_t).cast(),
                &mut length,
            )
        };
        if result == 0 && pid > 0 {
            return Ok(pid as u32);
        }
    }
    #[cfg(target_os = "linux")]
    {
        let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
        let mut length = std::mem::size_of_val(&credentials) as libc::socklen_t;
        // SAFETY: SO_PEERCRED writes the declared ucred structure.
        let result = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut credentials as *mut libc::ucred).cast(),
                &mut length,
            )
        };
        if result == 0 && credentials.pid > 0 {
            return Ok(credentials.pid as u32);
        }
    }
    Err(format!(
        "Cannot verify preview socket's peer process: {}",
        io::Error::last_os_error()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png() -> String {
        let mut data = Cursor::new(Vec::new());
        image::RgbaImage::new(2, 3)
            .write_to(&mut data, image::ImageFormat::Png)
            .unwrap();
        STANDARD.encode(data.into_inner())
    }

    #[test]
    fn malformed_or_mismatched_frames_are_rejected_before_ui_decode() {
        assert!(checked_png(&png(), 2, 3).is_ok());
        assert!(checked_png(&png(), 3, 2).is_err());
        assert!(checked_png("not base64", 2, 3).is_err());
        assert!(checked_png(&png(), 4096, 4096).is_err());
        assert!(checked_png(&png(), 0, 1).is_err());
    }

    #[test]
    fn endpoint_is_private_and_deleted_on_drop() {
        let endpoint = Endpoint::open().unwrap();
        let directory = endpoint.directory.clone();
        let path = endpoint.path.clone();
        assert_eq!(
            fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(endpoint);
        assert!(!path.exists());
        assert!(!directory.exists());
    }

    #[test]
    fn peer_identity_comes_from_kernel_instead_of_hello_claim() {
        let (left, _right) = UnixStream::pair().unwrap();
        assert_eq!(peer_pid(&left).unwrap(), std::process::id());
    }

    #[test]
    fn slow_reader_cannot_grow_outbound_input_without_bound() {
        let (stream, _reader) = UnixStream::pair().unwrap();
        let mut peer = Peer {
            stream,
            peer_pid: std::process::id(),
            authenticated: true,
            connected: Instant::now(),
            input: Vec::new(),
            output: Vec::new(),
        };
        let message = PreviewHostMessage::Input {
            input: PreviewInput::Text {
                text: "x".repeat(256 * 1024),
            },
        };
        assert!(peer.send(&message).is_err());
        assert!(peer.output.is_empty());
    }

    #[test]
    fn peer_must_authenticate_before_frames_and_cannot_forge_its_kernel_pid() {
        use std::os::unix::process::CommandExt;
        let child = std::process::Command::new("/bin/sleep")
            .arg("60")
            .process_group(0)
            .spawn()
            .unwrap();
        let group = child.id();
        let mut process = OwnedProcess {
            child,
            group,
            reaped: false,
        };
        let state = Arc::new(Mutex::new(PreviewState::new(
            super::super::PreviewMode::Cooperative,
        )));
        for (token_ok, pid_ok, protocol, hello) in [
            (false, true, PREVIEW_PROTOCOL_VERSION, true),
            (true, false, PREVIEW_PROTOCOL_VERSION, true),
            (true, true, 999, true),
            (true, true, PREVIEW_PROTOCOL_VERSION, false),
        ] {
            let mut endpoint = Endpoint::open().unwrap();
            let (stream, mut sender) = UnixStream::pair().unwrap();
            stream.set_nonblocking(true).unwrap();
            // The transport double supplies the pid that the kernel-bound
            // accept path already verified. The hello still cannot change it.
            endpoint.peer = Some(Peer {
                stream,
                peer_pid: group,
                authenticated: false,
                connected: Instant::now(),
                input: Vec::new(),
                output: Vec::new(),
            });
            let message = if hello {
                PreviewClientMessage::Hello {
                    token: if token_ok {
                        endpoint.token.clone()
                    } else {
                        "wrong-token".into()
                    },
                    pid: if pid_ok { group } else { std::process::id() },
                    protocol,
                }
            } else {
                PreviewClientMessage::Frame {
                    png: png(),
                    width: 2,
                    height: 3,
                }
            };
            let mut line = serde_json::to_vec(&message).unwrap();
            line.push(b'\n');
            sender.write_all(&line).unwrap();
            assert!(endpoint.tick_peer(&mut process, true, &state).is_err());
            assert!(state.lock().unwrap().frame.is_none());
        }
    }
}
