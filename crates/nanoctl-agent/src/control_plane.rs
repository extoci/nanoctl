use anyhow::{Context, Result, bail};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use url::Url;
use zeroize::Zeroizing;

use crate::platform::{self, Capabilities};

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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingSession {
    pub session_id: String,
    pub expires_at: u64,
    pub signals: Vec<SignalRow>,
}

#[derive(Debug, Deserialize)]
pub struct SignalRow {
    pub sequence: u64,
    pub sender: String,
    pub envelope: String,
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
        let client = Client::builder()
            .https_only(base_url.scheme() == "https")
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(30))
            .user_agent(concat!("nanoctl-agent/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self { base_url, client })
    }

    pub async fn enroll(&self, code: String, name: String) -> Result<Enrollment> {
        let response = self
            .client
            .post(self.endpoint("/v1/agent/enroll")?)
            .json(&EnrollmentRequest {
                code,
                name,
                platform: platform::PLATFORM,
                architecture: platform::ARCHITECTURE,
                agent_version: env!("CARGO_PKG_VERSION"),
                capabilities: platform::capabilities(),
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

    pub async fn heartbeat(&self, token: &Zeroizing<String>) -> Result<()> {
        self.client
            .post(self.endpoint("/v1/agent/heartbeat")?)
            .bearer_auth(token.as_str())
            .json(&serde_json::json!({
                "agentVersion": env!("CARGO_PKG_VERSION"),
                "capabilities": platform::capabilities(),
            }))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    pub async fn sessions(&self, token: &Zeroizing<String>) -> Result<SessionList> {
        self.client
            .get(self.endpoint("/v1/agent/sessions")?)
            .bearer_auth(token.as_str())
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
            .context("invalid session response")
    }

    #[cfg(feature = "media")]
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

    fn endpoint(&self, path: &str) -> Result<Url> {
        self.base_url
            .join(path)
            .context("invalid control-plane URL")
    }
}
