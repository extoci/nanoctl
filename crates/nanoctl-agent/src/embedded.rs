use std::{
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use async_trait::async_trait;
use directories::ProjectDirs;
use serde::Deserialize;
use serde_json::{Value, json};
use subtle::ConstantTimeEq;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, UdpSocket},
    process::{Child, ChildStdin, Command},
};

use crate::{
    config::{TailnetConfig, TlsMode},
    tailnet::{self, Identity, Node, Peer, Tailnet},
};

const HELPER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/nanoctl-tsnet"));
pub const BRIDGE_HEADER: &str = "x-nanoctl-bridge";
pub const PEER_HEADER: &str = "x-nanoctl-peer";

pub struct Helper {
    child: Child,
    stdin: ChildStdin,
    _executable: tempfile::TempDir,
}

impl Helper {
    fn new(mut child: Child, executable: tempfile::TempDir) -> Result<Self> {
        // Child::wait closes its own stdin; the helper treats EOF as a shutdown signal.
        let stdin = child.stdin.take().context("no helper stdin")?;
        Ok(Self {
            child,
            stdin,
            _executable: executable,
        })
    }

    pub async fn wait(&mut self) -> Result<()> {
        let status = self.child.wait().await?;
        bail!("embedded Tailscale exited unexpectedly ({status})");
    }

    pub async fn stop(mut self) {
        drop(self.stdin);
        if tokio::time::timeout(Duration::from_secs(3), self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.kill().await;
        }
    }
}

pub struct EmbeddedTailnet {
    control: SocketAddr,
    http: reqwest::Client,
    token: String,
    state_dir: PathBuf,
    #[cfg(feature = "rtc")]
    udp: Arc<UdpSocket>,
    #[cfg(not(feature = "rtc"))]
    _udp: UdpSocket,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Snapshot {
    node: Option<Value>,
    ready: bool,
    #[serde(rename = "HTTPS")]
    https: bool,
    port: u16,
    #[cfg(feature = "rtc")]
    media: String,
    #[cfg(feature = "rtc")]
    relay: u16,
    problem: String,
}

impl Snapshot {
    fn node(&self) -> Result<Node> {
        if !self.problem.is_empty() {
            bail!("{}", self.problem);
        }
        ensure!(self.ready, "embedded Tailscale is not connected");
        tailnet::parse_status(self.node.clone().context("no Tailscale status")?)
    }

    fn login(&self) -> Option<&str> {
        let login = self.node.as_ref()?.get("AuthURL")?.as_str()?;
        let parsed = url::Url::parse(login).ok()?;
        (parsed.scheme() == "https" && parsed.host_str() == Some("login.tailscale.com"))
            .then_some(login)
    }
}

fn state_dir() -> Result<PathBuf> {
    let project = ProjectDirs::from("dev", "nanoctl", "nanoctl")
        .context("operating system has no local data directory")?;
    Ok(project.data_local_dir().join("embedded-tailscale"))
}

fn snapshot(dir: &Path) -> Result<Snapshot> {
    let path = dir.join("viewer-status.json");
    let data = std::fs::read(&path)
        .context("embedded Tailscale is starting or stopped; start the nanoctl agent")?;
    ensure!(
        std::fs::metadata(path)?.modified()?.elapsed()? < Duration::from_secs(10),
        "embedded Tailscale is not responding; start or restart the nanoctl agent"
    );
    Ok(serde_json::from_slice(&data)?)
}

pub fn inspect(config: &TailnetConfig) -> Result<(Node, bool)> {
    let state = snapshot(&state_dir()?)?;
    ensure!(
        state.port == config.port && state.https == (config.tls == TlsMode::Auto),
        "restart the nanoctl agent to apply the embedded Tailscale configuration"
    );
    if let Some(login) = state.login() {
        bail!("sign into embedded Tailscale: {login}\nThen run nanoctl url again.");
    }
    Ok((state.node()?, state.https))
}

fn unpack_helper(root: &Path) -> Result<(tempfile::TempDir, Command)> {
    let dir = tempfile::Builder::new()
        .prefix("helper-")
        .tempdir_in(root)?;
    let binary = dir
        .path()
        .join(if cfg!(windows) { "tsnet.exe" } else { "tsnet" });
    std::fs::write(&binary, HELPER)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700))?;
    }
    let mut command = Command::new(binary);
    #[cfg(windows)]
    command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    command
        .env_remove("TS_AUTHKEY")
        .env_remove("TS_AUTH_KEY")
        .env_remove("TS_CLIENT_SECRET");
    Ok((dir, command))
}

pub async fn start(config: &TailnetConfig) -> Result<(Arc<EmbeddedTailnet>, TcpListener, Helper)> {
    let state_dir = state_dir()?;
    std::fs::create_dir_all(&state_dir)?;
    crate::tls::private(&state_dir, true)?;
    let _ = std::fs::remove_file(state_dir.join("viewer-status.json"));

    let (executable, mut command) = unpack_helper(&state_dir)?;
    let web = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let udp = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let token = uuid::Uuid::new_v4().simple().to_string();
    let http = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(3))
        .build()?;
    let child = command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("cannot start the embedded Tailscale helper")?;
    let mut helper = Helper::new(child, executable)?;
    let init = json!({
        "StateDir": state_dir,
        "Port": config.port,
        "HTTPS": config.tls == TlsMode::Auto,
        "Web": web.local_addr()?.port(),
        "UDP": udp.local_addr()?.port(),
        "Token": token,
    });
    helper
        .stdin
        .write_all(format!("{init}\n").as_bytes())
        .await?;
    let mut stdout = BufReader::new(helper.child.stdout.take().context("no helper stdout")?);
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(10), stdout.read_line(&mut line))
        .await
        .context("embedded Tailscale startup timed out")??;
    let control: SocketAddr = line
        .trim()
        .parse()
        .context("embedded Tailscale failed to start")?;
    ensure!(control.ip() == Ipv4Addr::LOCALHOST && control.port() != 0);
    let net = Arc::new(EmbeddedTailnet {
        control,
        http,
        token,
        state_dir,
        #[cfg(feature = "rtc")]
        udp: Arc::new(udp),
        #[cfg(not(feature = "rtc"))]
        _udp: udp,
    });
    Ok((net, web, helper))
}

impl EmbeddedTailnet {
    fn request(&self, path: &str) -> reqwest::RequestBuilder {
        let mut token = reqwest::header::HeaderValue::from_str(&self.token).unwrap();
        token.set_sensitive(true);
        self.http
            .get(format!("http://{}{path}", self.control))
            .header(BRIDGE_HEADER, token)
    }

    pub fn peer(&self, headers: &axum::http::HeaderMap) -> Option<SocketAddr> {
        bridge_peer(headers, &self.token)
    }
}

fn bridge_peer(headers: &axum::http::HeaderMap, token: &str) -> Option<SocketAddr> {
    let candidate = headers.get(BRIDGE_HEADER)?.as_bytes();
    if !bool::from(candidate.ct_eq(token.as_bytes())) {
        return None;
    }
    let peer = headers
        .get(PEER_HEADER)?
        .to_str()
        .ok()?
        .parse::<SocketAddr>()
        .ok()?;
    tailnet::is_tailnet_ip(peer.ip()).then_some(peer)
}

#[async_trait]
impl Tailnet for EmbeddedTailnet {
    async fn status(&self) -> Result<Node> {
        snapshot(&self.state_dir)?.node()
    }

    async fn whois(&self, peer: SocketAddr) -> Result<Identity> {
        tailnet::parse_whois(
            self.request("/whois")
                .query(&[("peer", peer.to_string())])
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?,
        )
    }

    async fn peers(&self) -> Result<Vec<Peer>> {
        tailnet::parse_peers(
            self.request("/peers")
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?,
        )
    }

    async fn reachable(&self, addr: SocketAddr) -> bool {
        self.request("/probe")
            .query(&[("ip", addr.ip())])
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
    }
}

#[cfg(feature = "rtc")]
pub mod ice;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue};

    #[test]
    #[ignore = "subprocess fixture for the helper lifetime test"]
    fn wait_for_lifetime_pipe_eof() {
        println!("lifetime pipe ready");
        std::io::copy(&mut std::io::stdin(), &mut std::io::sink()).unwrap();
        println!("lifetime pipe closed");
    }

    #[tokio::test]
    async fn monitoring_the_helper_keeps_its_lifetime_pipe_open_until_stop() {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "embedded::tests::wait_for_lifetime_pipe_eof",
                "--ignored",
                "--nocapture",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut helper = Helper::new(child, tempfile::tempdir().unwrap()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !stdout
                .next_line()
                .await
                .unwrap()
                .unwrap()
                .ends_with("lifetime pipe ready")
            {}
        })
        .await
        .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(300), helper.wait())
                .await
                .is_err(),
            "monitoring must not trigger EOF in the helper"
        );
        helper.stop().await;
        assert_eq!(
            stdout.next_line().await.unwrap().as_deref(),
            Some("lifetime pipe closed")
        );
    }

    #[tokio::test]
    async fn bundled_helper_runs_without_an_installed_tailscale() {
        let root = tempfile::tempdir().unwrap();
        let (dir, mut command) = unpack_helper(root.path()).unwrap();
        let expected = dir.path().to_owned();
        let result = command.arg("--version").output().await.unwrap();
        assert!(result.status.success());
        assert_eq!(
            String::from_utf8(result.stdout).unwrap().trim(),
            "nanoctl-tsnet"
        );
        drop(dir);
        assert!(!expected.exists());
    }

    #[test]
    fn bridge_authentication_never_trusts_local_or_unsealed_identity() {
        let mut headers = HeaderMap::new();
        headers.insert(PEER_HEADER, HeaderValue::from_static("100.100.0.1:7777"));
        assert!(bridge_peer(&headers, "secret").is_none());
        headers.insert(BRIDGE_HEADER, HeaderValue::from_static("wrong"));
        assert!(bridge_peer(&headers, "secret").is_none());
        headers.insert(BRIDGE_HEADER, HeaderValue::from_static("secret"));
        assert_eq!(bridge_peer(&headers, "secret").unwrap().port(), 7777);
        headers.insert(PEER_HEADER, HeaderValue::from_static("127.0.0.1:7777"));
        assert!(bridge_peer(&headers, "secret").is_none());
    }

    #[test]
    fn login_only_accepts_the_tailscale_sign_in_host() {
        let status = |auth: &str| Snapshot {
            node: Some(json!({"AuthURL": auth})),
            ready: false,
            https: true,
            port: 7337,
            #[cfg(feature = "rtc")]
            media: String::new(),
            #[cfg(feature = "rtc")]
            relay: 0,
            problem: String::new(),
        };
        assert!(
            status("https://login.tailscale.com/a/abc")
                .login()
                .is_some()
        );
        assert!(
            status("https://login.tailscale.com.attacker.test/")
                .login()
                .is_none()
        );
        assert!(status("http://login.tailscale.com/a/abc").login().is_none());
    }
}
