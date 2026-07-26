use std::fs::{self, OpenOptions};
#[cfg(unix)]
use std::io::Read;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use directories::ProjectDirs;
use ed25519_dalek::{Signature, VerifyingKey};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAX_ENVELOPE_BYTES: usize = 1_048_576;
const MAX_PAYLOAD_BYTES: usize = 524_288;
const MAX_ARTIFACT_BYTES: u64 = 268_435_456;
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

pub fn unix_time_now() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")
        .map(|duration| duration.as_secs())
}

pub fn default_staging_directory() -> Result<PathBuf> {
    let directories = ProjectDirs::from("dev", "nanoctl", "nanoctl")
        .context("operating system has no application data directory")?;
    Ok(directories.data_local_dir().join("updates"))
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

pub async fn stage_artifact(
    client: &reqwest::Client,
    artifact: &UpdateArtifact,
    staging_directory: &Path,
) -> Result<PathBuf> {
    validate_artifact(artifact)?;
    create_private_directory(staging_directory)?;
    let final_path = staging_directory.join(if cfg!(windows) {
        "nanoctl.next.exe"
    } else {
        "nanoctl.next"
    });
    if final_path.exists() {
        bail!("a staged update already exists at {}", final_path.display());
    }
    let temporary_path = staging_directory.join(format!(".download-{}.tmp", std::process::id()));
    let result = download_to_temporary(client, artifact, &temporary_path).await;
    if result.is_err() {
        let _ = fs::remove_file(&temporary_path);
        return result.map(|()| final_path);
    }
    fs::hard_link(&temporary_path, &final_path)
        .with_context(|| format!("cannot commit staged update at {}", final_path.display()))?;
    fs::remove_file(&temporary_path)
        .with_context(|| format!("cannot remove {}", temporary_path.display()))?;
    sync_directory(staging_directory)?;
    Ok(final_path)
}

pub fn activate_staged(artifact: &UpdateArtifact) -> Result<PathBuf> {
    #[cfg(windows)]
    bail!(
        "Windows activation requires the signed installer bootstrap; use Stage-Update then run the installer"
    );
    #[cfg(unix)]
    {
        let current = std::env::current_exe().context("cannot locate the running agent")?;
        let staged = default_staging_directory()?.join("nanoctl.next");
        activate_paths(&current, &staged, artifact)?;
        Ok(current)
    }
}

pub fn rollback_update() -> Result<PathBuf> {
    #[cfg(windows)]
    bail!("Windows rollback is handled by the signed installer bootstrap");
    #[cfg(unix)]
    {
        let current = std::env::current_exe().context("cannot locate the running agent")?;
        rollback_paths(&current)?;
        Ok(current)
    }
}

pub fn commit_update() -> Result<()> {
    #[cfg(windows)]
    bail!("Windows update commit is handled by the signed installer bootstrap");
    #[cfg(unix)]
    {
        let current = std::env::current_exe().context("cannot locate the running agent")?;
        for suffix in [".previous", ".failed"] {
            let retained = sibling_path(&current, suffix)?;
            if retained.exists() {
                require_regular_file(&retained, "retained agent")?;
                fs::remove_file(&retained)
                    .with_context(|| format!("cannot remove {}", retained.display()))?;
            }
        }
        sync_directory(
            current
                .parent()
                .context("installed agent has no parent directory")?,
        )
    }
}

async fn download_to_temporary(
    client: &reqwest::Client,
    artifact: &UpdateArtifact,
    temporary_path: &Path,
) -> Result<()> {
    let mut response = client
        .get(&artifact.url)
        .send()
        .await
        .context("update download failed")?
        .error_for_status()
        .context("update server rejected the download")?;
    if let Some(content_length) = response.content_length()
        && content_length != artifact.size
    {
        bail!("update Content-Length does not match the signed manifest");
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temporary_path)
        .with_context(|| format!("cannot create {}", temporary_path.display()))?;
    set_private_executable(&file)?;
    let mut digest = Sha256::new();
    let mut received = 0_u64;
    while let Some(chunk) = response
        .chunk()
        .await
        .context("update download interrupted")?
    {
        received = received
            .checked_add(chunk.len() as u64)
            .context("update size overflow")?;
        if received > artifact.size || received > MAX_ARTIFACT_BYTES {
            bail!("update download exceeds the signed size");
        }
        digest.update(&chunk);
        file.write_all(&chunk)
            .context("cannot write staged update")?;
    }
    validate_download(received, &digest.finalize(), artifact)?;
    file.sync_all().context("cannot persist staged update")?;
    Ok(())
}

fn validate_download(received: u64, digest: &[u8], artifact: &UpdateArtifact) -> Result<()> {
    if received != artifact.size {
        bail!("update download is shorter than the signed size");
    }
    let actual = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if !actual.eq_ignore_ascii_case(&artifact.sha256) {
        bail!("update SHA-256 does not match the signed manifest");
    }
    Ok(())
}

#[cfg(unix)]
fn verify_file(path: &Path, artifact: &UpdateArtifact) -> Result<()> {
    require_regular_file(path, "update candidate")?;
    let metadata =
        fs::metadata(path).with_context(|| format!("cannot inspect {}", path.display()))?;
    if metadata.len() != artifact.size || metadata.len() > MAX_ARTIFACT_BYTES {
        bail!("update candidate length does not match the signed manifest");
    }
    let mut file =
        fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut received = 0_u64;
    loop {
        let count = file
            .read(&mut buffer)
            .context("cannot read update candidate")?;
        if count == 0 {
            break;
        }
        received += count as u64;
        digest.update(&buffer[..count]);
    }
    validate_download(received, &digest.finalize(), artifact)
}

#[cfg(unix)]
fn require_regular_file(path: &Path, label: &str) -> Result<()> {
    if !fs::symlink_metadata(path)
        .with_context(|| format!("cannot inspect {label} at {}", path.display()))?
        .file_type()
        .is_file()
    {
        bail!("{label} must be a regular file");
    }
    Ok(())
}

#[cfg(unix)]
fn activate_paths(current: &Path, staged: &Path, artifact: &UpdateArtifact) -> Result<()> {
    require_regular_file(current, "installed agent")?;
    verify_file(staged, artifact)?;
    let candidate = sibling_path(current, ".candidate")?;
    let previous = sibling_path(current, ".previous")?;
    if candidate.exists() || previous.exists() {
        bail!("an activation or rollback file already exists beside the installed agent");
    }
    copy_new(staged, &candidate)?;
    verify_file(&candidate, artifact)?;
    fs::rename(current, &previous).context("cannot retain the installed agent for rollback")?;
    if let Err(error) = fs::rename(&candidate, current) {
        let rollback = fs::rename(&previous, current);
        if let Err(rollback_error) = rollback {
            return Err(error).context(format!(
                "activation and automatic rollback failed: {rollback_error}"
            ));
        }
        return Err(error).context("cannot activate update; installed agent was restored");
    }
    sync_directory(
        current
            .parent()
            .context("installed agent has no parent directory")?,
    )?;
    if let Err(error) = fs::remove_file(staged) {
        tracing::warn!(path = %staged.display(), %error, "activated staging file could not be removed");
    }
    Ok(())
}

#[cfg(unix)]
fn rollback_paths(current: &Path) -> Result<()> {
    require_regular_file(current, "installed agent")?;
    let previous = sibling_path(current, ".previous")?;
    require_regular_file(&previous, "rollback agent")?;
    let failed = sibling_path(current, ".failed")?;
    if failed.exists() {
        bail!("a failed update is already retained beside the installed agent");
    }
    fs::rename(current, &failed).context("cannot retain the failed agent")?;
    if let Err(error) = fs::rename(&previous, current) {
        let restore = fs::rename(&failed, current);
        if let Err(restore_error) = restore {
            return Err(error).context(format!("rollback and restoration failed: {restore_error}"));
        }
        return Err(error).context("cannot roll back; current agent was restored");
    }
    sync_directory(
        current
            .parent()
            .context("installed agent has no parent directory")?,
    )
}

#[cfg(unix)]
fn sibling_path(current: &Path, suffix: &str) -> Result<PathBuf> {
    let name = current
        .file_name()
        .context("installed agent has no file name")?;
    let mut sibling = name.to_os_string();
    sibling.push(suffix);
    Ok(current.with_file_name(sibling))
}

#[cfg(unix)]
fn copy_new(source: &Path, destination: &Path) -> Result<()> {
    let mut source =
        fs::File::open(source).with_context(|| format!("cannot open {}", source.display()))?;
    let mut destination = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .with_context(|| format!("cannot create {}", destination.display()))?;
    set_private_executable(&destination)?;
    std::io::copy(&mut source, &mut destination).context("cannot copy update candidate")?;
    destination
        .sync_all()
        .context("cannot persist update candidate")
}

fn create_private_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("cannot create {}", path.display()))?;
    if !fs::symlink_metadata(path)
        .with_context(|| format!("cannot inspect {}", path.display()))?
        .file_type()
        .is_dir()
    {
        bail!("update staging path is not a directory");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .context("cannot secure update staging directory")?;
    }
    Ok(())
}

fn set_private_executable(_file: &std::fs::File) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        _file
            .set_permissions(fs::Permissions::from_mode(0o700))
            .context("cannot secure staged update")?;
    }
    Ok(())
}

fn sync_directory(_path: &Path) -> Result<()> {
    #[cfg(unix)]
    std::fs::File::open(_path)
        .with_context(|| format!("cannot open {}", _path.display()))?
        .sync_all()
        .context("cannot persist update directory")?;
    Ok(())
}

fn verify_for_target(
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

fn validate_artifact(artifact: &UpdateArtifact) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use ed25519_dalek::{Signer, SigningKey};
    use serde_json::json;
    use sha2::Digest;

    use super::{UpdateArtifact, validate_download, verify_for_target};
    #[cfg(unix)]
    use super::{activate_paths, rollback_paths};

    fn signed_envelope(payload: serde_json::Value) -> (Vec<u8>, String) {
        let signing_key = SigningKey::from_bytes(&[7; 32]);
        let payload = serde_json::to_vec(&payload).unwrap();
        let signature = signing_key.sign(&payload);
        (
            serde_json::to_vec(&json!({
                "payload": STANDARD.encode(&payload),
                "signature": STANDARD.encode(signature.to_bytes()),
            }))
            .unwrap(),
            STANDARD.encode(signing_key.verifying_key().to_bytes()),
        )
    }

    fn manifest() -> serde_json::Value {
        json!({
            "format_version": 1,
            "version": "1.1.0",
            "published_at": 1_000,
            "expires_at": 2_000,
            "artifacts": [{
                "target": "linux-x86_64",
                "url": "https://updates.nanoctl.dev/v1.1.0/nanoctl",
                "size": 1234,
                "sha256": "ab".repeat(32),
            }],
        })
    }

    #[test]
    fn accepts_exact_signed_payload_for_target() {
        let (envelope, key) = signed_envelope(manifest());
        let artifact = verify_for_target(&envelope, &key, "1.0.0", 1_500, "linux-x86_64").unwrap();
        assert_eq!(artifact.size, 1234);
    }

    #[test]
    fn rejects_tampering_expiry_and_downgrade() {
        let (mut envelope, key) = signed_envelope(manifest());
        let last = envelope.len() - 2;
        envelope[last] ^= 1;
        assert!(verify_for_target(&envelope, &key, "1.0.0", 1_500, "linux-x86_64").is_err());

        let (envelope, key) = signed_envelope(manifest());
        assert!(verify_for_target(&envelope, &key, "1.0.0", 2_000, "linux-x86_64").is_err());
        assert!(verify_for_target(&envelope, &key, "1.1.0", 1_500, "linux-x86_64").is_err());
    }

    #[test]
    fn rejects_wrong_target_and_unsafe_url() {
        let (envelope, key) = signed_envelope(manifest());
        assert!(verify_for_target(&envelope, &key, "1.0.0", 1_500, "windows-x86_64").is_err());

        let mut unsafe_manifest = manifest();
        unsafe_manifest["artifacts"][0]["url"] = json!("http://updates.nanoctl.dev/nanoctl");
        let (envelope, key) = signed_envelope(unsafe_manifest);
        assert!(verify_for_target(&envelope, &key, "1.0.0", 1_500, "linux-x86_64").is_err());
    }

    #[test]
    fn requires_exact_download_size_and_digest() {
        let bytes = b"signed release bytes";
        let digest = sha2::Sha256::digest(bytes);
        let artifact = UpdateArtifact {
            target: "linux-x86_64".into(),
            url: "https://updates.nanoctl.dev/nanoctl".into(),
            size: bytes.len() as u64,
            sha256: digest.iter().map(|byte| format!("{byte:02x}")).collect(),
        };
        assert!(validate_download(bytes.len() as u64, &digest, &artifact).is_ok());
        assert!(validate_download(bytes.len() as u64 - 1, &digest, &artifact).is_err());
        assert!(validate_download(bytes.len() as u64, &[0; 32], &artifact).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn activation_retains_previous_and_rollback_restores_it() {
        let directory = tempfile::tempdir().unwrap();
        let current = directory.path().join("nanoctl");
        let staged = directory.path().join("staged");
        std::fs::write(&current, b"old signed binary").unwrap();
        std::fs::write(&staged, b"new signed binary").unwrap();
        let digest = sha2::Sha256::digest(b"new signed binary");
        let artifact = UpdateArtifact {
            target: "linux-x86_64".into(),
            url: "https://updates.nanoctl.dev/nanoctl".into(),
            size: 17,
            sha256: digest.iter().map(|byte| format!("{byte:02x}")).collect(),
        };

        activate_paths(&current, &staged, &artifact).unwrap();
        assert_eq!(std::fs::read(&current).unwrap(), b"new signed binary");
        assert_eq!(
            std::fs::read(directory.path().join("nanoctl.previous")).unwrap(),
            b"old signed binary"
        );
        assert!(!staged.exists());

        rollback_paths(&current).unwrap();
        assert_eq!(std::fs::read(&current).unwrap(), b"old signed binary");
        assert_eq!(
            std::fs::read(directory.path().join("nanoctl.failed")).unwrap(),
            b"new signed binary"
        );
    }
}
