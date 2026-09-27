use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use riwork_remote::{
    config::{Storage, private_read},
    relay::{Relay, Routes},
};
use std::{net::SocketAddr, path::PathBuf};

#[derive(Parser)]
#[command(
    name = "riwork-remote",
    version,
    about = "Encrypted relay and outbound desktop connector (standalone; no GPUI)"
)]
struct Cli {
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    /// Run blind plaintext router on loopback; terminate TLS at a reverse proxy.
    Relay {
        #[arg(long, default_value = "127.0.0.1:8787")]
        bind: SocketAddr,
        #[arg(long)]
        routes: PathBuf,
        #[arg(long, default_value_t = 256)]
        max_connections: usize,
    },
    /// Pair an individual device; sensitive export file must not already exist.
    Pair {
        #[arg(long)]
        relay: String,
        #[arg(long)]
        name: String,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        relay_routes: Option<PathBuf>,
        #[arg(long)]
        allow_insecure_loopback: bool,
        #[arg(long)]
        show_link: bool,
    },
    /// Revoke a device locally; live connector closes its access within one second.
    Revoke { device_id: String },
    /// List device metadata without secrets.
    Devices,
    /// Maintain outbound per-device connections; inherits RIWORK_HOME unchanged.
    Start {
        #[arg(long)]
        riwork: Option<PathBuf>,
    },
}
#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Action::Relay {
            bind,
            routes,
            max_connections,
        } => {
            let routes: Routes = private_read(&routes, 1024 * 1024)?;
            eprintln!("RiWork blind relay listening on {bind}; no payload logging.");
            Relay::new(routes, max_connections)?.serve(bind).await?
        }
        Action::Pair {
            relay,
            name,
            out,
            relay_routes,
            allow_insecure_loopback,
            show_link,
        } => {
            let p = Storage::from_env()?.pair(
                relay,
                name,
                allow_insecure_loopback,
                &out,
                relay_routes.as_deref(),
            )?;
            println!(
                "Paired device {}. Secret pairing JSON: {}",
                p.device_id,
                out.display()
            );
            if show_link {
                println!("{}", p.deep_link()?);
            }
            println!(
                "Provision the relay route hashes and start/restart the relay; then run riwork remote start."
            );
        }
        Action::Revoke { device_id } => {
            Storage::from_env()?.revoke(&device_id)?;
            println!(
                "Revoked {device_id}; active connector denies further requests. Remove relay route to invalidate routing tokens."
            );
        }
        Action::Devices => {
            let c = Storage::from_env()?.config()?;
            println!("{}",serde_json::to_string_pretty(&c.devices.iter().map(|d|serde_json::json!({"device_id":d.pairing.device_id,"device_name":d.pairing.device_name,"route_id":d.pairing.route_id,"revoked":d.revoked})).collect::<Vec<_>>())?);
        }
        Action::Start { riwork } => {
            let path = riwork
                .or_else(|| std::env::var_os("RIWORK_CLI").map(PathBuf::from))
                .or_else(|| {
                    std::env::current_exe()
                        .ok()
                        .and_then(|p| p.parent().map(|d| d.join("riwork")))
                })
                .context("pass --riwork /absolute/path/to/riwork or set RIWORK_CLI")?;
            riwork_remote::connector::start(Storage::from_env()?, path).await?;
        }
    }
    Ok(())
}
