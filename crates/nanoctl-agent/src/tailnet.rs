use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::config::TailnetConfig;

pub const CAPABILITY: &str = "extoci.lol/cap/nanoctl";

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    pub ips: Vec<IpAddr>,
    pub dns_name: String,
    pub host_name: String,
    pub owner: String,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Peer {
    pub host_name: String,
    pub dns_name: String,
    pub ips: Vec<IpAddr>,
    pub os: String,
    pub online: bool,
    pub tags: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub login: String,
    pub display_name: String,
    pub machine_name: String,
    pub tags: Vec<String>,
    #[serde(skip)]
    pub grants: Option<Vec<Value>>,
}

#[async_trait]
pub trait Tailnet: Send + Sync {
    async fn peers(&self) -> Result<Vec<Peer>> {
        Ok(Vec::new())
    }
    async fn status(&self) -> Result<Node>;
    async fn whois(&self, peer: SocketAddr) -> Result<Identity>;
    async fn cert(&self, _: &str, _: &std::path::Path, _: &std::path::Path) -> Result<()> {
        bail!("certificate issuance is unavailable");
    }
}

pub struct CliTailnet {
    path: PathBuf,
    cache: Mutex<HashMap<SocketAddr, (Instant, Identity)>>,
}

impl CliTailnet {
    pub fn discover(config: &TailnetConfig) -> Result<Self> {
        let path = if let Some(path) = &config.tailscale_cli {
            path.clone()
        } else {
            let mut paths = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .map(|path| {
                    path.join(if cfg!(windows) {
                        "tailscale.exe"
                    } else {
                        "tailscale"
                    })
                })
                .collect::<Vec<_>>();
            paths.push(PathBuf::from(
                "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
            ));
            if let Some(program_files) = std::env::var_os("ProgramFiles") {
                paths.push(
                    PathBuf::from(program_files)
                        .join("Tailscale")
                        .join("tailscale.exe"),
                );
            }
            paths.into_iter().find(|path| path.is_file()).context(
                "tailscale CLI not found; install Tailscale or set tailnet.tailscale_cli",
            )?
        };
        Ok(Self {
            path,
            cache: Mutex::new(HashMap::new()),
        })
    }

    async fn command(&self, args: &[&str]) -> Result<Value> {
        let output = tokio::time::timeout(
            Duration::from_secs(5),
            tokio::process::Command::new(&self.path)
                .args(args)
                .kill_on_drop(true)
                .output(),
        )
        .await
        .context("tailscale CLI timed out")??;
        if !output.status.success() {
            bail!("tailscale CLI failed; is tailscaled running and logged in?");
        }
        serde_json::from_slice(&output.stdout).context("invalid tailscale JSON")
    }
}

#[async_trait]
impl Tailnet for CliTailnet {
    async fn peers(&self) -> Result<Vec<Peer>> {
        parse_peers(self.command(&["status", "--json"]).await?)
    }
    async fn cert(&self, dns: &str, cert: &std::path::Path, key: &std::path::Path) -> Result<()> {
        crate::tls::validate_dns_name(dns)?;
        let output = tokio::time::timeout(
            Duration::from_secs(60),
            tokio::process::Command::new(&self.path)
                .arg("cert")
                .arg("--cert-file")
                .arg(cert)
                .arg("--key-file")
                .arg(key)
                .arg(dns)
                .kill_on_drop(true)
                .output(),
        )
        .await
        .context("tailscale cert timed out")??;
        if !output.status.success() {
            bail!("tailscale cert failed; {}", crate::tls::CERT_HINT);
        }
        Ok(())
    }
    async fn status(&self) -> Result<Node> {
        parse_status(self.command(&["status", "--json"]).await?)
    }

    async fn whois(&self, peer: SocketAddr) -> Result<Identity> {
        if let Some((time, identity)) = self.cache.lock().await.get(&peer)
            && time.elapsed() < Duration::from_secs(5)
        {
            return Ok(identity.clone());
        }
        let identity = parse_whois(
            self.command(&["whois", "--json", &peer.to_string()])
                .await?,
        )?;
        let mut cache = self.cache.lock().await;
        cache.retain(|_, (time, _)| time.elapsed() < Duration::from_secs(5));
        if cache.len() >= 1024 {
            cache.clear();
        }
        cache.insert(peer, (Instant::now(), identity.clone()));
        Ok(identity)
    }
}

pub fn is_tailnet_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => u32::from(ip) & 0xffc00000 == 0x64400000,
        IpAddr::V6(ip) => ip.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Status {
    backend_state: String,
    #[serde(rename = "Self")]
    node: StatusNode,
    #[serde(default)]
    user: HashMap<String, Profile>,
}

#[derive(Deserialize)]
struct StatusNode {
    #[serde(rename = "TailscaleIPs", default)]
    ips: Vec<IpAddr>,
    #[serde(rename = "DNSName", default)]
    dns_name: String,
    #[serde(rename = "HostName", default)]
    host_name: String,
    #[serde(rename = "UserID", default)]
    user_id: u64,
    #[serde(rename = "Tags", default, deserialize_with = "null_default")]
    tags: Vec<String>,
}

fn null_default<'de, D, T>(deserializer: D) -> std::result::Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct Profile {
    #[serde(default)]
    login_name: String,
    #[serde(default)]
    display_name: String,
}

pub fn parse_status(value: Value) -> Result<Node> {
    let status: Status = serde_json::from_value(value)?;
    if status.backend_state != "Running" {
        bail!("tailscale is not running");
    }
    let ips = status
        .node
        .ips
        .into_iter()
        .filter(|ip| is_tailnet_ip(*ip))
        .collect::<Vec<_>>();
    if ips.is_empty() {
        bail!("tailscale has no tailnet addresses");
    }
    let owner = if status.node.tags.is_empty() {
        status
            .user
            .get(&status.node.user_id.to_string())
            .map(|user| user.login_name.clone())
            .unwrap_or_default()
    } else {
        String::new()
    };
    Ok(Node {
        ips,
        dns_name: status.node.dns_name.trim_end_matches('.').to_owned(),
        host_name: status.node.host_name,
        owner,
    })
}

pub fn parse_peers(value: Value) -> Result<Vec<Peer>> {
    #[derive(Deserialize)]
    struct RawPeer {
        #[serde(flatten)]
        node: StatusNode,
        #[serde(rename = "OS", default)]
        os: String,
        #[serde(rename = "Online", default)]
        online: bool,
    }
    let peers: HashMap<String, RawPeer> = serde_json::from_value(
        value
            .get("Peer")
            .filter(|v| !v.is_null())
            .cloned()
            .unwrap_or_else(|| serde_json::json!({})),
    )?;
    Ok(peers
        .into_values()
        .filter_map(|peer| {
            let ips: Vec<_> = peer
                .node
                .ips
                .into_iter()
                .filter(|ip| is_tailnet_ip(*ip))
                .collect();
            (!ips.is_empty()).then(|| Peer {
                host_name: peer.node.host_name,
                dns_name: peer.node.dns_name.trim_end_matches('.').to_owned(),
                ips,
                os: peer.os,
                online: peer.online,
                tags: peer.node.tags,
            })
        })
        .collect())
}

pub fn parse_whois(value: Value) -> Result<Identity> {
    let profile: Profile = serde_json::from_value(
        value
            .get("UserProfile")
            .filter(|value| !value.is_null())
            .cloned()
            .unwrap_or_else(|| serde_json::json!({})),
    )?;
    let tags: Vec<String> = serde_json::from_value(
        value
            .pointer("/Node/Tags")
            .filter(|value| !value.is_null())
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
    )?;
    let machine_name = value
        .pointer("/Node/HostName")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let grants = value
        .get("CapMap")
        .and_then(|caps| caps.get(CAPABILITY))
        .map(|grants| serde_json::from_value(grants.clone()))
        .transpose()?;
    if profile.login_name.is_empty() && tags.is_empty() {
        bail!("peer has no tailnet identity");
    }
    Ok(Identity {
        login: profile.login_name,
        display_name: profile.display_name,
        machine_name,
        tags,
        grants,
    })
}

pub fn authorize(config: &TailnetConfig, node: &Node, peer: &Identity) -> Option<bool> {
    if !peer.tags.is_empty() && !peer.tags.iter().any(|tag| config.allow_tags.contains(tag)) {
        return None;
    }
    if let Some(grants) = &peer.grants {
        return Some(
            grants
                .iter()
                .any(|grant| grant.get("control") == Some(&Value::Bool(true))),
        );
    }
    if !peer.tags.is_empty() {
        return Some(true);
    }
    if !peer.login.is_empty()
        && (config.allow_users.contains(&peer.login)
            || (config.allow_users.is_empty() && peer.login == node.owner))
    {
        return Some(true);
    }
    None
}

struct LazyTailnet {
    config: TailnetConfig,
    cli: Mutex<Option<Arc<CliTailnet>>>,
}

impl LazyTailnet {
    async fn cli(&self) -> Result<Arc<CliTailnet>> {
        let mut cli = self.cli.lock().await;
        if cli.is_none() {
            *cli = Some(Arc::new(CliTailnet::discover(&self.config)?));
        }
        Ok(cli.as_ref().unwrap().clone())
    }
}

#[async_trait]
impl Tailnet for LazyTailnet {
    async fn peers(&self) -> Result<Vec<Peer>> {
        self.cli().await?.peers().await
    }
    async fn cert(&self, dns: &str, cert: &std::path::Path, key: &std::path::Path) -> Result<()> {
        self.cli().await?.cert(dns, cert, key).await
    }
    async fn status(&self) -> Result<Node> {
        self.cli().await?.status().await
    }
    async fn whois(&self, peer: SocketAddr) -> Result<Identity> {
        self.cli().await?.whois(peer).await
    }
}

pub fn real(config: &TailnetConfig) -> Arc<dyn Tailnet> {
    Arc::new(LazyTailnet {
        config: config.clone(),
        cli: Mutex::new(None),
    })
}

pub fn urls(node: &Node, port: u16) -> Vec<String> {
    let mut urls = node
        .ips
        .iter()
        .map(|ip| format!("http://{}/", SocketAddr::new(*ip, port)))
        .collect::<Vec<_>>();
    if !node.dns_name.is_empty() {
        urls.insert(0, format!("http://{}:{port}/", node.dns_name));
    }
    urls
}

#[cfg(test)]
pub struct FakeTailnet {
    pub node: Node,
    pub identity: Identity,
}

#[cfg(test)]
#[async_trait]
impl Tailnet for FakeTailnet {
    async fn status(&self) -> Result<Node> {
        Ok(self.node.clone())
    }
    async fn whois(&self, _: SocketAddr) -> Result<Identity> {
        Ok(self.identity.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn peer_samples() {
        assert!(parse_peers(json!({"Peer":null})).unwrap().is_empty());
        let peers = parse_peers(json!({"Peer": {
            "key:one":{"HostName":"desktop","DNSName":"desktop.tail.ts.net.","TailscaleIPs":["100.64.1.2","192.168.1.2"],"OS":"linux","Online":false,"Tags":["tag:desktop"]},
            "key:two":{"HostName":"invalid","TailscaleIPs":["192.168.1.3"],"Tags":null}
        }})).unwrap();
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].dns_name, "desktop.tail.ts.net");
        assert_eq!(peers[0].ips.len(), 1);
        assert!(!peers[0].online);
        assert_eq!(peers[0].tags, ["tag:desktop"]);
        assert!(parse_peers(json!({"Peer": []})).is_err());
    }

    #[test]
    fn status_and_whois_samples() {
        let node = parse_status(json!({"BackendState":"Running","Self":{"TailscaleIPs":["100.100.1.2","fd7a:115c:a1e0::1234"],"DNSName":"host.tail123.ts.net.","HostName":"host","UserID":123},"User":{"123":{"LoginName":"owner@example.com","DisplayName":"Owner"}}})).unwrap();
        assert_eq!(node.owner, "owner@example.com");
        assert_eq!(node.dns_name, "host.tail123.ts.net");
        assert_eq!(node.ips.len(), 2);
        let peer = parse_whois(json!({"Node":{"HostName":"friend-laptop","Tags": ["tag:ops"]},"UserProfile":{"LoginName":"tagged-devices","DisplayName":"Tagged Devices"},"CapMap":{CAPABILITY:[{"control":false}]}})).unwrap();
        assert_eq!(peer.tags, ["tag:ops"]);
        assert_eq!(peer.machine_name, "friend-laptop");
        assert!(peer.grants.is_some());
        assert!(parse_whois(json!({})).is_err());
        assert!(parse_status(json!({"BackendState":"Stopped"})).is_err());
    }

    #[test]
    fn policy() {
        let mut config = TailnetConfig::default();
        let node = Node {
            owner: "owner@example.com".into(),
            ..Default::default()
        };
        let mut peer = Identity {
            login: node.owner.clone(),
            ..Default::default()
        };
        assert_eq!(authorize(&config, &node, &peer), Some(true));
        peer.login = "other@example.com".into();
        assert_eq!(authorize(&config, &node, &peer), None);
        config.allow_users.push(peer.login.clone());
        assert_eq!(authorize(&config, &node, &peer), Some(true));
        peer.grants = Some(vec![json!({})]);
        assert_eq!(authorize(&config, &node, &peer), Some(false));
        peer.grants = Some(vec![json!({"control":false})]);
        assert_eq!(authorize(&config, &node, &peer), Some(false));
        peer.grants = Some(vec![json!({"control":true})]);
        assert_eq!(authorize(&config, &node, &peer), Some(true));
        peer.tags.push("tag:ops".into());
        assert_eq!(authorize(&config, &node, &peer), None);
        config.allow_tags.push("tag:ops".into());
        assert_eq!(authorize(&config, &node, &peer), Some(true));
        peer.grants = None;
        assert_eq!(authorize(&config, &node, &peer), Some(true));
    }
}
