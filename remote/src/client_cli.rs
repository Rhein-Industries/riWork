//! The subcommands that use another Mac as a client: `hosts`, `client`, `call`,
//! `status` and `attach`. They are flattened into `somtus-remote`'s own, so the
//! parser lives here and `main.rs` only dispatches.
use crate::{
    bridge,
    client::{HostsFile, RegistryAfter, add_host, remove_host},
    client_daemon::{
        DAEMON_PROTOCOL, DEFAULT_IDLE, daemon_call, daemon_status, daemon_watch, ensure, serve,
        shutdown, socket_path,
    },
    config::Storage,
    log_safe,
    ssh::{DEFAULT_REMOTE_BIN, RouteChange},
};
use anyhow::{Context, Result, bail};
use clap::Subcommand;
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};

#[derive(Subcommand)]
pub enum ClientCommand {
    /// Add, change, list or remove the Macs this Mac controls.
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
    /// Add an existing Somtus installation through SSH. Does not install software.
    AddSsh {
        #[arg(long)]
        target: String,
        #[arg(long, default_value = DEFAULT_REMOTE_BIN)]
        remote_bin: String,
        #[arg(long)]
        somtus: Option<String>,
        /// The remote RIWORK_HOME as an absolute path, if it differs from that account's default.
        #[arg(long)]
        home: Option<String>,
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Change how an SSH route connects, or its label, keeping its tabs and drafts. New connection values are tried first, and must reach the same installation.
    UpdateSsh {
        /// The route's id (`hosts list`).
        id: String,
        #[arg(long)]
        target: Option<String>,
        #[arg(long)]
        remote_bin: Option<String>,
        #[arg(long, conflicts_with = "no_somtus")]
        somtus: Option<String>,
        /// Stop passing --somtus to the host.
        #[arg(long)]
        no_somtus: bool,
        /// The remote RIWORK_HOME as an absolute path.
        #[arg(long, conflicts_with = "no_home")]
        home: Option<String>,
        /// Go back to that account's default RIWORK_HOME.
        #[arg(long)]
        no_home: bool,
        #[arg(long)]
        label: Option<String>,
        /// Print the route as a JSON object (as `hosts list --json` shows it).
        #[arg(long)]
        json: bool,
    },
    /// Pair with a host from the somtus://pair?v=2 link `pair --protocol 2 --kind desktop` printed there.
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
    /// Forget a host and its credentials; its client process quits. Also takes a record out of a registry that fails validation, by its exact id.
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
    /// Print this file's daemon protocol number, which `ensure` asks before it replaces a daemon with this file.
    #[command(hide = true)]
    Protocol,
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
    std::env::current_exe().context("cannot find the somtus-remote executable")
}

async fn hosts(action: HostsCommand) -> Result<()> {
    let storage = Storage::from_env()?;
    match action {
        HostsCommand::AddSsh {
            target,
            remote_bin,
            somtus,
            home,
            label,
            json,
        } => {
            let route = crate::ssh::Route {
                target,
                remote_bin,
                somtus,
                home,
                host_id: String::new(),
                client_id: uuid::Uuid::new_v4().to_string(),
            };
            let added = match crate::ssh::add(&storage, route, label).await {
                Ok(added) => added,
                Err(error) => {
                    // This command runs on the person's own terminal, which may see
                    // what ssh printed. The exit keeps that after the error.
                    crate::ssh::print_dial_error(&error);
                    std::process::exit(1);
                }
            };
            let host = &added.host;
            if json {
                println!("{}", serde_json::to_string(host)?);
            } else {
                println!(
                    "Added SSH route {} (\"{}\") to host {}.",
                    host.id,
                    host.label,
                    host.host_id.as_deref().unwrap_or("")
                );
            }
            for warning in &added.warnings {
                eprintln!("Warning: {warning}");
            }
        }
        HostsCommand::UpdateSsh {
            id,
            target,
            remote_bin,
            somtus,
            no_somtus,
            home,
            no_home,
            label,
            json,
        } => {
            let change = RouteChange {
                target,
                remote_bin,
                somtus: if no_somtus {
                    Some(None)
                } else {
                    somtus.map(Some)
                },
                home: if no_home { Some(None) } else { home.map(Some) },
                label,
            };
            let updated = match crate::ssh::update(&storage, &id, change).await {
                Ok(updated) => updated,
                Err(error) => {
                    // As for `add-ssh`: this is the person's own terminal.
                    crate::ssh::print_dial_error(&error);
                    std::process::exit(1);
                }
            };
            let host = &updated.host;
            if json {
                println!("{}", serde_json::to_string(host)?);
            } else if updated.saved {
                println!(
                    "Updated SSH route {} (\"{}\") to host {}.",
                    host.id,
                    host.label,
                    host.host_id.as_deref().unwrap_or("")
                );
            } else {
                println!(
                    "SSH route {} (\"{}\") already has these values.",
                    host.id, host.label
                );
            }
            for warning in &updated.warnings {
                eprintln!("Warning: {warning}");
            }
            // Its client process holds the route it read, and would go on dialing it until
            // it next reads the registry. A label is no reason to end its terminals.
            if updated.route_changed {
                shutdown(&storage, &id).await?;
            }
        }
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
                println!(
                    "No hosts. Add one with `hosts add --link 'somtus://pair?v=2&…'` or `hosts add-ssh --target SSH_TARGET`."
                );
            } else {
                // The label last, as it can hold spaces. So can a route's paths, which are
                // for `--json`.
                for host in hosts {
                    let identity = host.host_id.as_deref().unwrap_or(&host.relay);
                    println!(
                        "{}  {}  {}  {}",
                        host.id, host.transport, identity, host.label
                    );
                }
            }
        }
        HostsCommand::Remove { id } => {
            let removal = remove_host(&storage, &id)?;
            // Printed first: it is the only copy left, whatever happens next.
            for host in &removal.hosts {
                println!(
                    "Removed host {} (\"{}\").",
                    log_safe(&host.id),
                    log_safe(&host.label)
                );
                if let Some(command) = host.add_command() {
                    println!("To add it again: {command}");
                    if host.home_is_not_absolute() {
                        println!(
                            "Its home is not an absolute path, so add-ssh will need the home's absolute path on the host instead."
                        );
                    }
                } else if let Some(device) = &host.device_id {
                    println!(
                        "The host still trusts this Mac, as device {device}, until `somtus remote revoke {device}` runs there."
                    );
                }
            }
            match &removal.registry {
                RegistryAfter::Valid => {}
                RegistryAfter::Repaired => println!("The host registry is valid again."),
                RegistryAfter::StillInvalid(reason) => {
                    eprintln!("Warning: the host registry is still invalid: {reason}");
                }
            }
            // Its client process would otherwise go on using the credentials until it next
            // reads the registry. Only a UUID can have one, and a damaged file can name a
            // host anything.
            if crate::crypto::uuid(&id).is_ok() {
                shutdown(&storage, &id).await?;
            }
        }
    }
    Ok(())
}

async fn daemon(action: DaemonCommand) -> Result<()> {
    match action {
        DaemonCommand::Serve {
            desktop,
            idle_seconds,
        } => {
            let idle = Duration::from_secs(idle_seconds.max(1));
            serve(Storage::from_env()?, &desktop, idle).await
        }
        DaemonCommand::Ensure { desktop } => {
            ensure(&Storage::from_env()?, &desktop, &current_exe()?).await?;
            Ok(())
        }
        DaemonCommand::Socket { desktop } => {
            println!(
                "{}",
                socket_path(&Storage::from_env()?, &desktop)?.display()
            );
            Ok(())
        }
        // Says what the file is, so it has no use for a home, which may be in any state.
        DaemonCommand::Protocol => {
            println!("{DAEMON_PROTOCOL}");
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
        println!(
            "{}",
            escape_c1(&serde_json::to_string_pretty(&reply["result"])?)
        );
        return Ok(());
    }
    // The host's words, on its way to a terminal.
    bail!(
        "{}: {}",
        log_safe(reply["error"]["code"].as_str().unwrap_or("error")),
        log_safe(
            reply["error"]["message"]
                .as_str()
                .unwrap_or("the call failed")
        )
    )
}

/// JSON text with DEL and the C1 controls escaped as well, which a terminal acts on as it
/// does on the C0 ones that JSON escapes by itself. The text means the same: JSON has
/// these only inside strings. Its own line breaks stay.
fn escape_c1(json: &str) -> String {
    json.chars()
        .map(|c| {
            if ('\u{7f}'..='\u{9f}').contains(&c) {
                format!("\\u{:04x}", u32::from(c))
            } else {
                c.to_string()
            }
        })
        .collect()
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
