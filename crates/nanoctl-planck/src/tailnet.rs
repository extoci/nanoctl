use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Host {
    pub name: String,
    pub dns_name: String,
    pub os: String,
    pub local: bool,
    pub secure_url: String,
    pub insecure_url: String,
}

#[derive(Deserialize)]
struct Status {
    #[serde(rename = "Self")]
    node: StatusNode,
    #[serde(rename = "Peer", default)]
    peers: HashMap<String, StatusNode>,
}

#[derive(Clone, Deserialize)]
struct StatusNode {
    #[serde(rename = "HostName", default)]
    host_name: String,
    #[serde(rename = "DNSName", default)]
    dns_name: String,
    #[serde(rename = "TailscaleIPs", default)]
    ips: Vec<IpAddr>,
    #[serde(rename = "OS", default)]
    os: String,
    #[serde(rename = "Online", default)]
    online: bool,
}

pub fn discover(include_peers: bool, port: u16) -> Result<Vec<Host>> {
    let cli = tailscale_cli()?;
    let output = Command::new(cli)
        .args(["status", "--json"])
        .output()
        .context("cannot run tailscale status")?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        bail!(
            "tailscale status failed{}",
            if detail.is_empty() {
                "; is Tailscale running and signed in?".to_owned()
            } else {
                format!(": {detail}")
            }
        );
    }
    let status: Status =
        serde_json::from_slice(&output.stdout).context("invalid tailscale JSON")?;
    let mut nodes = vec![(status.node, true)];
    if include_peers {
        nodes.extend(
            status
                .peers
                .into_values()
                .filter(|peer| peer.online)
                .map(|peer| (peer, false)),
        );
    }
    nodes.retain(|(node, _)| node.ips.iter().any(|ip| is_tailnet_ip(*ip)));

    let probes = std::thread::scope(|scope| {
        nodes
            .iter()
            .map(|(node, _)| scope.spawn(|| probe_node(node, port)))
            .collect::<Vec<_>>()
            .into_iter()
            .map(|probe| probe.join().unwrap_or(false))
            .collect::<Vec<_>>()
    });

    let mut hosts = nodes
        .into_iter()
        .zip(probes)
        .filter_map(|((node, local), listening)| {
            listening.then(|| host_for_node(node, local, port))
        })
        .collect::<Vec<_>>();
    hosts.sort_by(|a, b| (!a.local).cmp(&(!b.local)).then(a.name.cmp(&b.name)));
    Ok(hosts)
}

fn tailscale_cli() -> Result<PathBuf> {
    let executable = if cfg!(windows) {
        "tailscale.exe"
    } else {
        "tailscale"
    };
    let mut paths = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|path| path.join(executable))
        .collect::<Vec<_>>();
    if let Some(program_files) = std::env::var_os("ProgramFiles") {
        paths.push(
            PathBuf::from(program_files)
                .join("Tailscale")
                .join("tailscale.exe"),
        );
    }
    paths.push(PathBuf::from(
        "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
    ));
    paths
        .into_iter()
        .find(|path| path.is_file())
        .context("Tailscale CLI was not found; install Tailscale and sign in")
}

fn is_tailnet_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => u32::from(ip) & 0xffc00000 == 0x64400000,
        IpAddr::V6(ip) => ip.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    }
}

fn probe_node(node: &StatusNode, port: u16) -> bool {
    node.ips.iter().filter(|ip| is_tailnet_ip(**ip)).any(|ip| {
        TcpStream::connect_timeout(&SocketAddr::new(*ip, port), Duration::from_millis(300)).is_ok()
    })
}

fn host_for_node(mut node: StatusNode, local: bool, port: u16) -> Host {
    node.dns_name = node.dns_name.trim_end_matches('.').to_owned();
    node.ips.retain(|ip| is_tailnet_ip(*ip));
    let ip = node.ips.first().copied();
    let host = if node.dns_name.is_empty() {
        ip.map(|ip| ip.to_string()).unwrap_or_default()
    } else {
        node.dns_name.clone()
    };
    let address = host_port(&host, port);
    let fallback_address = ip
        .map(|ip| SocketAddr::new(ip, port).to_string())
        .unwrap_or_else(|| address.clone());
    Host {
        name: if node.host_name.is_empty() {
            host.clone()
        } else {
            node.host_name
        },
        dns_name: node.dns_name,
        os: if node.os.is_empty() {
            "unknown".to_owned()
        } else {
            node.os
        },
        local,
        secure_url: format!("https://{address}/"),
        insecure_url: format!("http://{fallback_address}/"),
    }
}

fn host_port(host: &str, port: u16) -> String {
    if let Ok(ip) = host.parse::<IpAddr>() {
        SocketAddr::new(ip, port).to_string()
    } else {
        format!("{host}:{port}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tailnet_ip_recognizes_v4_and_v6_ranges() {
        assert!(is_tailnet_ip("100.64.0.1".parse().unwrap()));
        assert!(is_tailnet_ip("fd7a:115c:a1e0::1".parse().unwrap()));
        assert!(!is_tailnet_ip("192.168.1.10".parse().unwrap()));
    }

    #[test]
    fn host_urls_prefer_dns_for_tls_and_ip_for_http_fallback() {
        let node = StatusNode {
            host_name: "desktop".to_owned(),
            dns_name: "desktop.tail.ts.net.".to_owned(),
            ips: vec!["100.64.1.2".parse().unwrap()],
            os: "windows".to_owned(),
            online: true,
        };
        let host = host_for_node(node, false, 7337);
        assert_eq!(host.secure_url, "https://desktop.tail.ts.net:7337/");
        assert_eq!(host.insecure_url, "http://100.64.1.2:7337/");
    }

    #[test]
    fn status_shape_deserializes() {
        let status: Status = serde_json::from_value(json!({
            "Self": {"HostName":"local","DNSName":"local.tail.ts.net.","TailscaleIPs":["100.64.1.1"]},
            "Peer": {"key":{"HostName":"peer","DNSName":"peer.tail.ts.net.","TailscaleIPs":["100.64.1.2"],"OS":"windows","Online":true}}
        }))
        .unwrap();
        assert_eq!(status.node.host_name, "local");
        assert_eq!(status.peers["key"].dns_name, "peer.tail.ts.net.");
    }
}
