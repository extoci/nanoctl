use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signature, VerifyingKey};
use semver::Version;
use serde::{Deserialize, Serialize};

const MAX_ENVELOPE_BYTES: usize = 1_048_576;
const MAX_PAYLOAD_BYTES: usize = 524_288;
pub const MAX_ARTIFACT_BYTES: u64 = 268_435_456;
const MAX_VALIDITY_SECONDS: u64 = 31 * 24 * 60 * 60;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedEnvelope {
    payload: String,
    signature: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateManifest {
    format_version: u8,
    version: String,
    published_at: u64,
    expires_at: u64,
    artifacts: Vec<UpdateArtifact>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateArtifact {
    pub target: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,
}

pub fn verify_for_current_target(
    envelope_bytes: &[u8],
    public_key_base64: &str,
    current_version: &str,
    now: u64,
) -> Result<UpdateArtifact> {
    verify_for_target(
        envelope_bytes,
        public_key_base64,
        current_version,
        now,
        &format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
    )
}

pub fn verify_for_target(
    envelope_bytes: &[u8],
    public_key_base64: &str,
    current_version: &str,
    now: u64,
    target: &str,
) -> Result<UpdateArtifact> {
    if envelope_bytes.len() > MAX_ENVELOPE_BYTES {
        bail!("update manifest envelope exceeds the size limit");
    }
    let envelope: SignedEnvelope =
        serde_json::from_slice(envelope_bytes).context("update envelope is invalid")?;
    let payload = STANDARD
        .decode(envelope.payload)
        .context("update payload is not valid base64")?;
    if payload.len() > MAX_PAYLOAD_BYTES {
        bail!("signed update payload exceeds the size limit");
    }
    let public_key = decode_array::<32>(public_key_base64, "update public key")?;
    let signature = decode_array::<64>(&envelope.signature, "update signature")?;
    let verifying_key =
        VerifyingKey::from_bytes(&public_key).context("update public key is invalid")?;
    verifying_key
        .verify_strict(&payload, &Signature::from_bytes(&signature))
        .context("update manifest signature is invalid")?;

    let manifest: UpdateManifest =
        serde_json::from_slice(&payload).context("signed update payload is invalid")?;
    validate_manifest(&manifest, current_version, now)?;
    let mut matching = manifest
        .artifacts
        .into_iter()
        .filter(|item| item.target == target);
    let artifact = matching
        .next()
        .with_context(|| format!("manifest has no artifact for {target}"))?;
    if matching.next().is_some() {
        bail!("manifest has duplicate artifacts for {target}");
    }
    validate_artifact(&artifact)?;
    Ok(artifact)
}

fn validate_manifest(manifest: &UpdateManifest, current_version: &str, now: u64) -> Result<()> {
    if manifest.format_version != 1 {
        bail!("unsupported update manifest format");
    }
    if manifest.expires_at <= manifest.published_at
        || manifest.expires_at - manifest.published_at > MAX_VALIDITY_SECONDS
    {
        bail!("update manifest validity window is invalid");
    }
    if now < manifest.published_at || now >= manifest.expires_at {
        bail!("update manifest is not currently valid");
    }
    let offered = Version::parse(&manifest.version).context("update version is invalid")?;
    let current = Version::parse(current_version).context("current version is invalid")?;
    if offered <= current {
        bail!("update version must be newer than the installed version");
    }
    if manifest.artifacts.is_empty() || manifest.artifacts.len() > 16 {
        bail!("update manifest artifact count is invalid");
    }
    Ok(())
}

pub fn validate_artifact(artifact: &UpdateArtifact) -> Result<()> {
    if artifact.size == 0 || artifact.size > MAX_ARTIFACT_BYTES {
        bail!("update artifact size is invalid");
    }
    let url = url::Url::parse(&artifact.url).context("update artifact URL is invalid")?;
    if url.scheme() != "https" || url.username() != "" || url.password().is_some() {
        bail!("update artifact URL must be credential-free HTTPS");
    }
    if artifact.sha256.len() != 64 || !artifact.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("update artifact SHA-256 is invalid");
    }
    Ok(())
}

fn decode_array<const N: usize>(encoded: &str, label: &str) -> Result<[u8; N]> {
    let decoded = STANDARD
        .decode(encoded)
        .with_context(|| format!("{label} is not valid base64"))?;
    decoded
        .try_into()
        .map_err(|_| anyhow::anyhow!("{label} must contain exactly {N} bytes"))
}
