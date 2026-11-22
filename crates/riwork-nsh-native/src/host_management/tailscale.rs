use nsh_core::prelude::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap, io::ErrorKind, net::IpAddr, path::Path, process::Stdio, time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command, time::timeout};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct TailscaleMac {
    pub id: String,
    pub label: String,
    pub target: String,
    pub dns_name: Option<String>,
    pub online: bool,
}

// Only deserialize fields used to identify owned Macs. Status also contains private
// account and network information that is never included in the native API.
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Status {
    backend_state: String,
    #[serde(rename = "Self")]
    local: Option<Node>,
    peer: Option<BTreeMap<String, Option<Node>>>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
struct Node {
    #[serde(rename = "ID")]
    id: String,
    #[serde(rename = "OS")]
    os: String,
    #[serde(rename = "UserID")]
    owner: u64,
    #[serde(rename = "AltSharerUserID")]
    alt_sharer: u64,
    sharee_node: bool,
    tags: Option<Vec<String>>,
    host_name: String,
    #[serde(rename = "DNSName")]
    dns_name: String,
    #[serde(rename = "TailscaleIPs")]
    addresses: Option<Vec<IpAddr>>,
    online: bool,
}

impl Node {
    fn personally_owned(&self) -> bool {
        self.owner != 0
            && self.alt_sharer == 0
            && !self.sharee_node
            && self.tags.as_ref().is_none_or(Vec::is_empty)
    }
}

fn dns_name(value: &str) -> Option<String> {
    let name = value.strip_suffix('.').unwrap_or(value);
    (name.len() <= 253
        && name.split('.').all(|part| {
            !part.is_empty()
                && part.len() <= 63
                && !part.starts_with('-')
                && !part.ends_with('-')
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        }))
    .then(|| name.to_owned())
}

fn owned_macs(bytes: &[u8]) -> Result<Vec<TailscaleMac>, String> {
    let status: Status = serde_json::from_slice(bytes).map_err(|_| {
        "Open Tailscale and check that its CLI can run `tailscale status --json`.".to_string()
    })?;
    if status.backend_state != "Running" {
        return Err("Connect this Mac to Tailscale, then refresh Hosts.".into());
    }
    let local = status
        .local
        .filter(|node| !node.id.is_empty() && node.personally_owned())
        .ok_or(
            "Tailscale did not report a personal owner for this Mac. Add an SSH host manually.",
        )?;
    let mut macs = Vec::new();
    for node in status.peer.unwrap_or_default().into_values().flatten() {
        if node.id.is_empty()
            || node.id == local.id
            || node.owner != local.owner
            || !node.personally_owned()
            || !(node.os.eq_ignore_ascii_case("macOS") || node.os.eq_ignore_ascii_case("darwin"))
        {
            continue;
        }
        let addresses = node.addresses.unwrap_or_default();
        let Some(ip) = addresses
            .iter()
            .find(|ip| ip.is_ipv4())
            .or_else(|| addresses.first())
        else {
            continue;
        };
        let target = ip.to_string();
        let dns_name = dns_name(&node.dns_name);
        let host_name = node.host_name.trim();
        let label = if !host_name.is_empty()
            && host_name.chars().count() <= 64
            && !host_name.chars().any(char::is_control)
        {
            host_name
        } else {
            dns_name
                .as_deref()
                .and_then(|name| name.split('.').next())
                .unwrap_or(&target)
        };
        macs.push(TailscaleMac {
            id: node.id,
            label: label.to_owned(),
            target,
            dns_name,
            online: node.online,
        });
    }
    macs.sort_by_cached_key(|mac| (!mac.online, mac.label.to_lowercase(), mac.id.clone()));
    Ok(macs)
}

pub(super) async fn discover() -> Result<Vec<TailscaleMac>, String> {
    // Apps launched from Finder often have neither Homebrew nor /usr/local on PATH.
    let executable = [
        "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
        "/opt/homebrew/bin/tailscale",
        "/usr/local/bin/tailscale",
    ]
    .into_iter()
    .find(|path| Path::new(path).is_file())
    .unwrap_or("tailscale");
    timeout(Duration::from_secs(5), read_peers(executable))
        .await
        .map_err(|_| {
            "Tailscale took too long to respond. Check its connection and refresh Hosts."
                .to_string()
        })?
}

async fn read_peers(executable: &str) -> Result<Vec<TailscaleMac>, String> {
    const MAX_BYTES: u64 = 16 * 1024 * 1024;
    let mut command = Command::new(executable);
    #[cfg(target_os = "macos")]
    command.env("TAILSCALE_BE_CLI", "1");
    let mut child = command
        .args(["status", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| {
            if error.kind() == ErrorKind::NotFound {
                "Install Tailscale on this Mac or add an SSH host manually.".to_owned()
            } else {
                format!("Could not start the Tailscale CLI: {error}")
            }
        })?;
    let mut bytes = Vec::new();
    child
        .stdout
        .take()
        .expect("piped stdout")
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| format!("Could not read Tailscale status: {error}"))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(
            "The Tailscale status is too large to display. Add an SSH host manually.".into(),
        );
    }
    if !child
        .wait()
        .await
        .map_err(|error| format!("Could not read Tailscale status: {error}"))?
        .success()
    {
        return Err("Open Tailscale, connect this Mac, and refresh Hosts.".into());
    }
    owned_macs(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn exposes_discovery_in_the_native_host_management_api() {
        let types = nsh_core::element::declarations(&crate::registry(), &[]);
        assert!(types.contains("discoverTailscaleMacs(): Promise<(TailscaleMac)[]>"));
        assert!(types.contains("dnsName?: string | null"));
    }

    #[test]
    fn lists_only_other_personal_macs_and_selects_literal_ssh_addresses() {
        let mac = json!({"ID":"n1","HostName":"Studio","OS":"macOS","UserID":21,"TailscaleIPs":["fd7a:115c:a1e0::1","100.90.0.1"],"DNSName":"studio.example.ts.net.","Online":true});
        let mut peers = serde_json::Map::new();
        for (id, changes) in [
            ("self", json!({})),
            ("studio", json!({})),
            (
                "offline",
                json!({"OS":"darwin","Online":false,"HostName":"Notebook","DNSName":"-invalid","TailscaleIPs":["fd7a:115c:a1e0::2"]}),
            ),
            ("someone", json!({"UserID":22})),
            ("unknown", json!({"UserID":0})),
            ("shared", json!({"ShareeNode":true})),
            ("reshared", json!({"AltSharerUserID":27})),
            ("tag", json!({"Tags":["tag:work"]})),
            ("iphone", json!({"OS":"iOS"})),
            ("no-address", json!({"TailscaleIPs":null})),
        ] {
            let mut peer = mac.clone();
            peer["ID"] = json!(id);
            peer.as_object_mut()
                .unwrap()
                .extend(changes.as_object().unwrap().clone());
            peers.insert(id.into(), peer);
        }
        let status =
            json!({"Self":{"ID":"self","UserID":21},"BackendState":"Running","Peer":peers});
        let macs = owned_macs(&serde_json::to_vec(&status).unwrap()).unwrap();
        assert_eq!(
            macs.iter().map(|mac| mac.id.as_str()).collect::<Vec<_>>(),
            ["studio", "offline"]
        );
        assert_eq!(macs[0].target, "100.90.0.1");
        assert_eq!(macs[0].dns_name.as_deref(), Some("studio.example.ts.net"));
        assert_eq!(macs[1].target, "fd7a:115c:a1e0::2");
        assert_eq!(macs[1].dns_name, None);
    }

    #[test]
    fn distinguishes_empty_peer_directory_from_disconnected_or_unowned_self() {
        let status = json!({"Self":{"ID":"self","UserID":42},"BackendState":"Running","Peer":null});
        assert!(
            owned_macs(&serde_json::to_vec(&status).unwrap())
                .unwrap()
                .is_empty()
        );
        for invalid in [
            json!({"BackendState":"NeedsLogin","Self":null}),
            json!({"BackendState":"Running","Self":{"ID":"self"}}),
            json!({"BackendState":"Running","Self":{"ID":"self","UserID":42,"Tags":["tag:work"]}}),
        ] {
            assert!(owned_macs(&serde_json::to_vec(&invalid).unwrap()).is_err());
        }
        assert!(owned_macs(b"The Tailscale CLI failed to start.").is_err());
    }
}
