use anyhow::{Context, Result, bail};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use url::Url;
use zeroize::Zeroizing;

use crate::{
    config::EncoderPreference,
    platform::{self, Capabilities},
};

#[derive(Clone)]
pub struct ControlPlane {
    base_url: Url,
    client: Client,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Enrollment {
    pub device_id: String,
    pub token: String,
}

#[derive(Debug, Deserialize)]
pub struct SessionList {
    pub sessions: Vec<PendingSession>,
}

#[derive(Debug)]
pub enum SessionPollError {
    Revoked,
    Request(anyhow::Error),
}

impl std::fmt::Display for SessionPollError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Revoked => formatter.write_str("agent credential was revoked"),
            Self::Request(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for SessionPollError {}

impl From<anyhow::Error> for SessionPollError {
    fn from(error: anyhow::Error) -> Self {
        Self::Request(error)
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingSession {
    pub session_id: String,
    pub expires_at: u64,
    #[cfg(feature = "rtc")]
    pub controller_epoch: Option<String>,
    #[cfg(feature = "rtc")]
    pub host_epoch: Option<String>,
    pub signals: Vec<SignalRow>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignalRow {
    pub sequence: u64,
    pub sender: String,
    pub envelope: String,
    #[serde(default)]
    #[cfg(feature = "rtc")]
    pub created_at: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg(feature = "rtc")]
pub struct TurnCredentials {
    pub urls: Vec<String>,
    pub username: String,
    pub credential: String,
    pub expires_at: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EnrollmentRequest {
    code: String,
    name: String,
    platform: &'static str,
    architecture: &'static str,
    agent_version: &'static str,
    capabilities: Capabilities,
}

impl ControlPlane {
    pub fn new(base_url: Url) -> Result<Self> {
        install_crypto_provider()?;
        let client = Client::builder()
            .https_only(base_url.scheme() == "https")
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(30))
            .user_agent(concat!("nanoctl-agent/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self { base_url, client })
    }

    pub async fn enroll(
        &self,
        code: String,
        name: String,
        remote_input_enabled: bool,
        encoder_preference: EncoderPreference,
    ) -> Result<Enrollment> {
        let response = self
            .client
            .post(self.endpoint("/v1/agent/enroll")?)
            .json(&EnrollmentRequest {
                code,
                name,
                platform: platform::PLATFORM,
                architecture: platform::ARCHITECTURE,
                agent_version: env!("CARGO_PKG_VERSION"),
                capabilities: platform::capabilities(remote_input_enabled, encoder_preference),
            })
            .send()
            .await?;
        if response.status() == StatusCode::UNAUTHORIZED {
            bail!("setup code is invalid, expired, or already used");
        }
        response
            .error_for_status()?
            .json()
            .await
            .context("invalid enrollment response")
    }

    pub async fn heartbeat(
        &self,
        token: &Zeroizing<String>,
        heartbeat_seconds: u64,
        remote_input_enabled: bool,
        encoder_preference: EncoderPreference,
    ) -> Result<()> {
        self.client
            .post(self.endpoint("/v1/agent/heartbeat")?)
            .bearer_auth(token.as_str())
            .json(&serde_json::json!({
                "agentVersion": env!("CARGO_PKG_VERSION"),
                "heartbeatSeconds": heartbeat_seconds,
                "capabilities": platform::capabilities(remote_input_enabled, encoder_preference),
            }))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    pub async fn sessions(
        &self,
        token: &Zeroizing<String>,
    ) -> std::result::Result<SessionList, SessionPollError> {
        let response = self
            .client
            .get(self.endpoint("/v1/agent/sessions")?)
            .bearer_auth(token.as_str())
            .send()
            .await
            .map_err(anyhow::Error::from)
            .map_err(SessionPollError::Request)?;
        if credential_was_rejected(response.status()) {
            return Err(SessionPollError::Revoked);
        }
        response
            .error_for_status()
            .map_err(anyhow::Error::from)?
            .json()
            .await
            .context("invalid session response")
            .map_err(SessionPollError::Request)
    }

    #[cfg(feature = "rtc")]
    pub async fn send_signal(
        &self,
        token: &Zeroizing<String>,
        session_id: &str,
        sequence: u64,
        envelope: &str,
    ) -> Result<()> {
        self.client
            .post(self.endpoint("/v1/agent/signal")?)
            .bearer_auth(token.as_str())
            .json(&serde_json::json!({
                "sessionId": session_id,
                "sequence": sequence,
                "envelope": envelope,
            }))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    #[cfg(feature = "rtc")]
    pub async fn claim_host(
        &self,
        token: &Zeroizing<String>,
        session_id: &str,
        controller_epoch: &str,
        host_epoch: &str,
        expected_host_epoch: Option<&str>,
        generation: u64,
    ) -> Result<()> {
        self.client
            .post(self.endpoint("/v1/agent/claim-host")?)
            .bearer_auth(token.as_str())
            .json(&serde_json::json!({
                "sessionId": session_id,
                "controllerEpoch": controller_epoch,
                "hostEpoch": host_epoch,
                "expectedHostEpoch": expected_host_epoch,
                "generation": generation,
            }))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    #[cfg(feature = "rtc")]
    pub async fn turn_credentials(
        &self,
        token: &Zeroizing<String>,
        session_id: &str,
    ) -> Result<Option<TurnCredentials>> {
        let mut url = self.endpoint("/v1/agent/turn")?;
        url.query_pairs_mut().append_pair("sessionId", session_id);
        let response = self
            .client
            .get(url)
            .bearer_auth(token.as_str())
            .send()
            .await?
            .error_for_status()?;
        if response.status() == StatusCode::NO_CONTENT {
            return Ok(None);
        }
        response
            .json()
            .await
            .map(Some)
            .context("invalid TURN credential response")
    }

    fn endpoint(&self, path: &str) -> Result<Url> {
        self.base_url
            .join(path)
            .context("invalid control-plane URL")
    }
}

pub fn install_crypto_provider() -> Result<()> {
    static INITIALIZED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    INITIALIZED.get_or_init(|| {
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            let _ = rustls::crypto::ring::default_provider().install_default();
        }
    });
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        bail!("nanoctl could not initialize its built-in TLS crypto provider");
    }
    Ok(())
}

fn credential_was_rejected(status: StatusCode) -> bool {
    matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
}

#[cfg(test)]
mod tests {
    use super::{ControlPlane, credential_was_rejected};
    use reqwest::StatusCode;
    use url::Url;

    #[tokio::test]
    async fn heartbeat_reports_configured_interval() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(10)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 4096];
            loop {
                let count = stream.read(&mut buffer).unwrap();
                assert!(count > 0);
                request.extend_from_slice(&buffer[..count]);
                if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..end]);
                    let length: usize = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse().unwrap())
                        })
                        .unwrap();
                    if request.len() >= end + 4 + length {
                        let body: serde_json::Value =
                            serde_json::from_slice(&request[end + 4..end + 4 + length]).unwrap();
                        stream
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                            .unwrap();
                        return body;
                    }
                }
            }
        });
        let client = ControlPlane::new(Url::parse(&format!("http://{address}")).unwrap()).unwrap();
        client
            .heartbeat(
                &zeroize::Zeroizing::new("test-token".into()),
                60,
                false,
                crate::config::EncoderPreference::Auto,
            )
            .await
            .unwrap();
        assert_eq!(server.join().unwrap()["heartbeatSeconds"], 60);
    }

    #[test]
    fn constructs_https_client_with_a_crypto_provider() {
        ControlPlane::new(Url::parse("https://example.com").unwrap())
            .expect("the release TLS backend must include a crypto provider");
    }

    #[test]
    fn distinguishes_revocation_from_retryable_control_plane_failures() {
        assert!(credential_was_rejected(StatusCode::UNAUTHORIZED));
        assert!(credential_was_rejected(StatusCode::FORBIDDEN));
        assert!(!credential_was_rejected(StatusCode::TOO_MANY_REQUESTS));
        assert!(!credential_was_rejected(StatusCode::SERVICE_UNAVAILABLE));
    }
}
