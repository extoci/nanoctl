use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use rustls::{
    ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
};
use x509_parser::{extensions::GeneralName, parse_x509_certificate};

use crate::{
    config::{AgentConfig, TailnetConfig, TlsMode},
    tailnet::{Node, Tailnet},
};

pub const CERT_HINT: &str = "enable HTTPS certificates in the Tailscale admin DNS settings (https://login.tailscale.com/admin/dns), and permit the agent user to run tailscale cert (configure the tailscaled operator or run the agent with the required permissions)";
const RENEW_BEFORE: i64 = 14 * 24 * 60 * 60;

pub struct TlsState {
    pub config: Option<Arc<ServerConfig>>,
    pub detail: String,
}

struct StagingDirectory(PathBuf);

impl Drop for StagingDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn validate_dns_name(dns: &str) -> Result<()> {
    if dns.len() > 253
        || !dns.ends_with(".ts.net")
        || dns.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
    {
        bail!("invalid MagicDNS certificate name");
    }
    Ok(())
}

pub fn cert_paths(root: &Path, dns: &str) -> Result<(PathBuf, PathBuf)> {
    validate_dns_name(dns)?;
    Ok((
        root.join(format!("{dns}.crt")),
        root.join(format!("{dns}.key")),
    ))
}

pub fn needs_renewal(expiry: Option<i64>, now: i64) -> bool {
    expiry.is_none_or(|expiry| expiry.saturating_sub(now) <= RENEW_BEFORE)
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn directory() -> Result<PathBuf> {
    Ok(AgentConfig::default_path()?
        .parent()
        .context("configuration has no parent")?
        .join("tls"))
}

fn private(path: &Path, directory: bool) -> Result<()> {
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        bail!("TLS path must not be a symlink");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            path,
            fs::Permissions::from_mode(if directory { 0o700 } else { 0o600 }),
        )?;
    }
    #[cfg(windows)]
    {
        let output = std::process::Command::new("whoami").output()?;
        if !output.status.success() {
            bail!("cannot determine TLS file owner");
        }
        let user = String::from_utf8(output.stdout)?.trim().to_owned();
        let grant = format!("{user}:{}", if directory { "(OI)(CI)F" } else { "F" });
        let status = std::process::Command::new("icacls")
            .arg(path)
            .args(["/inheritance:r", "/grant:r", &grant])
            .output()?;
        if !status.status.success() {
            bail!("cannot restrict TLS file permissions");
        }
    }
    Ok(())
}

fn load(cert: &Path, key: &Path, dns: &str) -> Result<(Arc<ServerConfig>, i64)> {
    private(cert, false)?;
    private(key, false)?;
    let chain = CertificateDer::pem_file_iter(cert)?.collect::<std::result::Result<Vec<_>, _>>()?;
    let leaf = chain.first().context("empty TLS certificate chain")?;
    let (_, parsed) = parse_x509_certificate(leaf.as_ref())
        .map_err(|_| anyhow::anyhow!("invalid TLS certificate"))?;
    let matches_name = parsed.subject_alternative_name()?.is_some_and(|san| {
        san.value
            .general_names
            .iter()
            .any(|name| matches!(name, GeneralName::DNSName(name) if *name == dns))
    });
    if !matches_name || parsed.validity().not_before.timestamp() > now() {
        bail!("TLS certificate is not valid for this MagicDNS name");
    }
    let expiry = parsed.validity().not_after.timestamp();
    if expiry <= now() {
        bail!("TLS certificate has expired");
    }
    let config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()?
            .with_no_client_auth()
            .with_single_cert(chain, PrivateKeyDer::from_pem_file(key)?)?;
    Ok((Arc::new(config), expiry))
}

pub fn inspect(config: &TailnetConfig, node: &Node) -> TlsState {
    if config.tls == TlsMode::Off {
        return TlsState {
            config: None,
            detail: "disabled; using HTTP".into(),
        };
    }
    let result = (|| {
        let root = directory()?;
        private(&root, true)?;
        let (cert, key) = cert_paths(&root, &node.dns_name)?;
        load(&cert, &key, &node.dns_name)
    })();
    match result {
        Ok((tls, _)) => TlsState {
            config: Some(tls),
            detail: "native HTTPS certificate available".into(),
        },
        Err(error) => TlsState {
            config: None,
            detail: format!("HTTP fallback: {error}; {CERT_HINT}"),
        },
    }
}

pub async fn prepare(config: &TailnetConfig, node: &Node, tailnet: &dyn Tailnet) -> TlsState {
    if config.tls == TlsMode::Off {
        return inspect(config, node);
    }
    let result = async {
        let root = directory()?;
        fs::create_dir_all(&root)?;
        private(&root, true)?;
        let (cert, key) = cert_paths(&root, &node.dns_name)?;
        let cached = load(&cert, &key, &node.dns_name).ok();
        if !needs_renewal(cached.as_ref().map(|(_, expiry)| *expiry), now()) {
            return Ok(cached.unwrap().0);
        }
        let staging = root.join(uuid::Uuid::new_v4().to_string());
        fs::create_dir(&staging)?;
        let cleanup = StagingDirectory(staging.clone());
        private(&staging, true)?;
        let issued = async {
            let (new_cert, new_key) = cert_paths(&staging, &node.dns_name)?;
            tailnet.cert(&node.dns_name, &new_cert, &new_key).await?;
            let (tls, _) = load(&new_cert, &new_key, &node.dns_name)?;
            fs::rename(new_cert, &cert)?;
            fs::rename(new_key, &key)?;
            Ok::<_, anyhow::Error>(tls)
        }
        .await;
        drop(cleanup);
        match (issued, cached) {
            (Ok(tls), _) => Ok(tls),
            (Err(error), Some((tls, _))) => {
                tracing::warn!(%error, "certificate renewal failed; retaining valid certificate");
                Ok(tls)
            }
            (Err(error), None) => Err(error),
        }
    }
    .await;
    match result {
        Ok(tls) => TlsState {
            config: Some(tls),
            detail: "native HTTPS active".into(),
        },
        Err(error) => TlsState {
            config: None,
            detail: format!("HTTP fallback: {error}; {CERT_HINT}"),
        },
    }
}

pub fn urls(node: &Node, port: u16, tls: bool) -> Vec<String> {
    if tls {
        vec![format!("https://{}:{port}/", node.dns_name)]
    } else {
        crate::tailnet::urls(node, port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn certificate_loading_checks_identity_key_and_permissions() {
        let root = tempfile::tempdir().unwrap();
        let dns = "host.tail123.ts.net";
        let (cert, key) = cert_paths(root.path(), dns).unwrap();
        assert!(load(&cert, &key, dns).is_err());
        let issued = rcgen::generate_simple_self_signed(vec![dns.into()]).unwrap();
        fs::write(&cert, issued.cert.pem()).unwrap();
        fs::write(&key, issued.key_pair.serialize_pem()).unwrap();
        let (_, expiry) = load(&cert, &key, dns).unwrap();
        assert!(!needs_renewal(Some(expiry), now()));
        assert!(load(&cert, &key, "other.tail123.ts.net").is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&key).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        fs::write(&key, "invalid key").unwrap();
        assert!(load(&cert, &key, dns).is_err());
    }

    #[test]
    fn urls_follow_tls_mode() {
        let node = Node {
            dns_name: "host.tail123.ts.net".into(),
            ..Default::default()
        };
        assert_eq!(
            urls(&node, 7337, true),
            ["https://host.tail123.ts.net:7337/"]
        );
        assert_eq!(
            urls(&node, 7337, false),
            ["http://host.tail123.ts.net:7337/"]
        );
        assert_eq!(TailnetConfig::default().tls, TlsMode::Auto);
        assert!(toml::from_str::<TailnetConfig>("tls = 'invalid'").is_err());
        assert_eq!(
            toml::from_str::<TailnetConfig>("tls = 'off'").unwrap().tls,
            TlsMode::Off
        );
    }

    #[test]
    fn certificate_paths_validate_dns_and_stay_private() {
        let root = Path::new("private-state").join("tls");
        let (cert, key) = cert_paths(&root, "host.tail123.ts.net").unwrap();
        assert_eq!(cert, root.join("host.tail123.ts.net.crt"));
        assert_eq!(key, root.join("host.tail123.ts.net.key"));
        for dns in [
            "",
            "../host.ts.net",
            "-host.ts.net",
            "host..ts.net",
            "host.ts.net/evil",
            "--help",
            "host.example.com",
            "host.ts.net\n",
        ] {
            assert!(cert_paths(&root, dns).is_err(), "{dns:?}");
        }
    }

    #[test]
    fn certificate_renewal_boundaries() {
        let now = 1_700_000_000;
        assert!(needs_renewal(None, now));
        assert!(needs_renewal(Some(now - 1), now));
        assert!(needs_renewal(Some(now), now));
        assert!(needs_renewal(Some(now + RENEW_BEFORE), now));
        assert!(!needs_renewal(Some(now + RENEW_BEFORE + 1), now));
    }
}
