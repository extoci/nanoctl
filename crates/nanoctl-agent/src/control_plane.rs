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
    pub signals: Vec<SignalRow>,
}

#[derive(Debug, Deserialize)]
pub struct SignalRow {
    pub sequence: u64,
    pub sender: String,
    pub envelope: String,
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
                capabilities: platform::capabilities(remote_input_enabled),
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
        remote_input_enabled: bool,
    ) -> Result<()> {
        self.client
            .post(self.endpoint("/v1/agent/heartbeat")?)
            .bearer_auth(token.as_str())
            .json(&serde_json::json!({
                "agentVersion": env!("CARGO_PKG_VERSION"),
                "capabilities": platform::capabilities(remote_input_enabled),
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

fn credential_was_rejected(status: StatusCode) -> bool {
    matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
}

#[cfg(test)]
mod tests {
    use super::credential_was_rejected;
    use reqwest::StatusCode;

    #[test]
    fn distinguishes_revocation_from_retryable_control_plane_failures() {
        assert!(credential_was_rejected(StatusCode::UNAUTHORIZED));
        assert!(credential_was_rejected(StatusCode::FORBIDDEN));
        assert!(!credential_was_rejected(StatusCode::TOO_MANY_REQUESTS));
        assert!(!credential_was_rejected(StatusCode::SERVICE_UNAVAILABLE));
    }
}
