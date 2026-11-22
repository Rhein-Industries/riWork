//! Local Tailscale discovery and verified SSH registration.
mod tailscale;
pub use tailscale::TailscaleMac;

use crate::{remote, service};
use nsh_core::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct SshRoute {
    pub id: String,
    pub label: String,
    pub target: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[prop(exact)]
pub struct AddSshRequest {
    pub target: String,
    pub label: String,
    #[serde(default)]
    #[prop(skip_none)]
    pub remote_bin: Option<String>,
    #[serde(default)]
    #[prop(skip_none)]
    pub somtus: Option<String>,
}

#[module]
pub struct HostManagement;
#[module]
impl HostManagement {
    /// Read the local Tailscale directory without opening any SSH connections.
    pub async fn discover_tailscale_macs() -> Result<Vec<TailscaleMac>, String> {
        tailscale::discover().await
    }
    /// List saved SSH routes without connecting to their hosts.
    pub async fn ssh_routes() -> Result<Vec<SshRoute>, String> {
        service::blocking(|| remote::process()?.ssh_routes()).await
    }
    /// Register one host after the companion verifies SSH and the Somtus installation.
    pub async fn add_ssh(request: AddSshRequest) -> Result<(), String> {
        service::blocking(move || {
            let remote = remote::process()?;
            remote.add_ssh(&request)?;
            remote.poll();
            Ok(())
        })
        .await
    }
}
