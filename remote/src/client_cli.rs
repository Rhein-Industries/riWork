//! The subcommands that use another Mac as a client: `hosts`, `client`, `call`,
//! `status` and `attach`. They are flattened into `riwork-remote`'s own, so the
//! parser lives here and `main.rs` only dispatches.
use crate::{
    bridge,
    client::{HostsFile, add_host, remove_host},
    client_daemon::{
        DEFAULT_IDLE, daemon_call, daemon_status, daemon_watch, ensure, serve, shutdown,
        socket_path,
    },
    config::Storage,
};
use anyhow::{Context, Result, bail};
use clap::Subcommand;
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};

#[derive(Subcommand)]
pub enum ClientCommand {
    /// Add, list or remove the Macs this Mac controls.
    Hosts {
        #[command(subcommand)]
        action: HostsCommand,
    },
    /// The background process that holds the connection to one host.
    Client {
        #[command(subcommand)]
        action: DaemonCommand,
    },
    /// Call one RPC on a host and print its result as JSON; starts the client process if needed.
    Call {
        /// The host's desktop id (`hosts list`).
        #[arg(long)]
        desktop: String,
        /// For example projects.list or shell.create.
        method: String,
        /// The request parameters as a JSON object.
        #[arg(long)]
        params: Option<String>,
        #[arg(long, default_value_t = 15_000)]
        timeout_ms: u64,
    },
    /// Show whether a host is reachable; starts the client process if needed.
    Status {
        #[arg(long)]
        desktop: String,
        /// Keep printing as the state changes.
        #[arg(long)]
        watch: bool,
        /// One JSON object per line instead of a sentence.
        #[arg(long)]
        json: bool,
    },
    /// Show a shell of a host in this terminal; this is what a terminal tab runs.
    Attach {
        #[arg(long)]
        desktop: String,
        /// The shell's full UUID on the host.
        #[arg(long)]
        shell: String,
        /// Do not let this terminal's size resize the host's tmux window.
        #[arg(long)]
        ignore_size: bool,
    },
}

#[derive(Subcommand)]
pub enum HostsCommand {
    /// Pair with a host from the riwork://pair?v=2 link `pair --protocol 2 --kind desktop` printed there.
    Add {
        /// The link, or `-` to read it from stdin, which keeps the secret out of `ps`.
        #[arg(long)]
        link: String,
        /// A name to show for the host.
        #[arg(long)]
        label: Option<String>,
        /// Accept a plaintext ws:// relay on loopback (development only).
        #[arg(long)]
        allow_insecure_loopback: bool,
        /// Print the new host as a JSON object (as `hosts list --json` shows it).
        #[arg(long)]
        json: bool,
    },
    /// List the hosts, without secrets.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Forget a host and its credentials; its client process quits.
    Remove { id: String },
}

#[derive(Subcommand)]
pub enum DaemonCommand {
    /// Hold the connection to a host and serve this Mac's terminals and requests; exits when idle.
    Serve {
        #[arg(long)]
        desktop: String,
        /// Exit after this many seconds without a client.
        #[arg(long, default_value_t = DEFAULT_IDLE.as_secs(), hide = true)]
        idle_seconds: u64,
    },
    /// Start the client process for a host unless one runs. Safe to repeat.
    Ensure {
        #[arg(long)]
        desktop: String,
    },
    /// Print the path of the host's client socket; starts nothing.
    Socket {
        #[arg(long)]
        desktop: String,
    },
}

pub async fn run(command: ClientCommand) -> Result<()> {
    match command {
        ClientCommand::Hosts { action } => hosts(action).await,
        ClientCommand::Client { action } => daemon(action).await,
        ClientCommand::Call {
            desktop,
            method,
            params,
            timeout_ms,
        } => call(&desktop, &method, params.as_deref(), timeout_ms).await,
        ClientCommand::Status {
            desktop,
            watch,
            json,
        } => status(&desktop, watch, json).await,
        ClientCommand::Attach {
            desktop,
            shell,
            ignore_size,
        } => {
            let storage = Storage::from_env()?;
            let code =
                bridge::run(&storage, &desktop, &shell, ignore_size, &current_exe()?).await?;
            // The reader threads of the terminal would keep a normal return waiting.
            std::process::exit(code);
        }
    }
}

/// The link from standard input: one line, however it was piped (a trailing newline
/// is not part of it).
fn read_link_from_stdin() -> Result<String> {
    use std::io::Read;
    const MAX: u64 = 16 * 1024;
    let mut text = String::new();
    std::io::stdin()
        .take(MAX + 1)
        .read_to_string(&mut text)
        .context("read the pairing link from standard input")?;
    if text.len() as u64 > MAX {
        bail!("pairing link is too large");
    }
    Ok(text.trim().to_owned())
}

fn current_exe() -> Result<PathBuf> {
    std::env::current_exe().context("cannot find the riwork-remote executable")
}

async fn hosts(action: HostsCommand) -> Result<()> {
    let storage = Storage::from_env()?;
    match action {
        HostsCommand::Add {
            link,
            label,
            allow_insecure_loopback,
            json,
        } => {
            let link = if link == "-" {
                read_link_from_stdin()?
            } else {
                link
            };
            let added =
                add_host(&storage, &link, label.as_deref(), allow_insecure_loopback).await?;
            if json {
                println!("{}", serde_json::to_string(&added.host)?);
            } else {
                println!("Added host {} (\"{}\").", added.host.id, added.host.label);
            }
            if let Some(warning) = &added.warning {
                eprintln!("Warning: {warning}");
            } else if added.features.is_some_and(|f| f.pty.is_none()) {
                eprintln!(
                    "Warning: the host does not offer terminal streams to this Mac. Pair again there with `pair --protocol 2 --kind desktop`."
                );
            }
        }
        HostsCommand::List { json } => {
            let hosts: Vec<_> = HostsFile::load(&storage)?
                .hosts
                .iter()
                .map(|h| h.summary())
                .collect();
            if json {
                println!("{}", serde_json::to_string_pretty(&hosts)?);
            } else if hosts.is_empty() {
                println!("No hosts. Add one with `hosts add --link 'riwork://pair?v=2&…'`.");
            } else {
                for host in hosts {
                    println!("{}  {}  {}", host.id, host.label, host.relay);
                }
            }
        }
        HostsCommand::Remove { id } => {
            let removed = remove_host(&storage, &id)?;
            // Its client process would otherwise go on using the credentials until it idles out.
            shutdown(&storage, &removed.id).await?;
            println!("Removed host {} (\"{}\").", removed.id, removed.label);
        }
    }
    Ok(())
}

async fn daemon(action: DaemonCommand) -> Result<()> {
    let storage = Storage::from_env()?;
    match action {
        DaemonCommand::Serve {
            desktop,
            idle_seconds,
        } => serve(storage, &desktop, Duration::from_secs(idle_seconds.max(1))).await,
        DaemonCommand::Ensure { desktop } => {
            ensure(&storage, &desktop, &current_exe()?).await?;
            Ok(())
        }
        DaemonCommand::Socket { desktop } => {
            println!("{}", socket_path(&storage, &desktop)?.display());
            Ok(())
        }
    }
}

async fn call(desktop: &str, method: &str, params: Option<&str>, timeout_ms: u64) -> Result<()> {
    let params: Value = match params {
        Some(text) => serde_json::from_str(text).context("--params must be a JSON object")?,
        None => json!({}),
    };
    if !params.is_object() {
        bail!("--params must be a JSON object");
    }
    let storage = Storage::from_env()?;
    let socket = ensure(&storage, desktop, &current_exe()?).await?;
    let reply = daemon_call(&socket, method, params, timeout_ms).await?;
    if reply["ok"] == true {
        println!("{}", serde_json::to_string_pretty(&reply["result"])?);
        return Ok(());
    }
    bail!(
        "{}: {}",
        reply["error"]["code"].as_str().unwrap_or("error"),
        reply["error"]["message"]
            .as_str()
            .unwrap_or("the call failed")
    )
}

/// A status line as a sentence.
fn describe(status: &Value) -> String {
    let label = status["label"].as_str().unwrap_or("host");
    match status["state"].as_str() {
        Some("online") => match status["rtt_ms"].as_u64() {
            Some(ms) => format!("online ({ms} ms)  {label}"),
            None => format!("online  {label}"),
        },
        Some("offline") => {
            let since = status["since"].as_u64().unwrap_or(0);
            let for_s = crate::client::now_unix().saturating_sub(since);
            let reason = status["reason"].as_str().unwrap_or("unknown reason");
            format!("offline for {for_s} s ({reason})  {label}")
        }
        _ => format!("connecting  {label}"),
    }
}

async fn status(desktop: &str, watch: bool, json: bool) -> Result<()> {
    let storage = Storage::from_env()?;
    let socket = ensure(&storage, desktop, &current_exe()?).await?;
    let show = |status: &Value| {
        if json {
            println!("{status}");
        } else {
            println!("{}", describe(status));
        }
    };
    if watch {
        daemon_watch(&socket, |line| {
            show(&line);
            true
        })
        .await
    } else {
        show(&daemon_status(&socket).await?);
        Ok(())
    }
}
