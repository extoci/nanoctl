use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentConfig {
    pub control_plane_url: Url,
    pub device_id: Option<String>,
    pub quality: QualityConfig,
    pub network: NetworkConfig,
    pub features: FeatureConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct QualityConfig {
    pub codec: CodecPreference,
    pub max_fps: u16,
    pub max_bitrate_kbps: u32,
    pub max_width: u32,
    pub max_height: u32,
    pub latency_mode: LatencyMode,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct NetworkConfig {
    pub ice_transport: IceTransport,
    pub heartbeat_seconds: u64,
    pub poll_milliseconds: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct FeatureConfig {
    pub system_audio: bool,
    pub clipboard: bool,
    pub remote_input: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CodecPreference {
    #[default]
    Auto,
    H264,
    Vp9,
    Av1,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LatencyMode {
    Responsiveness,
    #[default]
    Balanced,
    Quality,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum IceTransport {
    #[default]
    All,
    Relay,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            control_plane_url: Url::parse("http://127.0.0.1:3211").expect("static URL is valid"),
            device_id: None,
            quality: QualityConfig::default(),
            network: NetworkConfig::default(),
            features: FeatureConfig::default(),
        }
    }
}

impl Default for QualityConfig {
    fn default() -> Self {
        Self {
            codec: CodecPreference::Auto,
            max_fps: 60,
            max_bitrate_kbps: 24_000,
            max_width: 3840,
            max_height: 2160,
            latency_mode: LatencyMode::Balanced,
        }
    }
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            ice_transport: IceTransport::All,
            heartbeat_seconds: 15,
            poll_milliseconds: 750,
        }
    }
}

impl Default for FeatureConfig {
    fn default() -> Self {
        Self {
            system_audio: false,
            clipboard: true,
            remote_input: true,
        }
    }
}

impl AgentConfig {
    pub fn default_path() -> Result<PathBuf> {
        let dirs = ProjectDirs::from("dev", "nanoctl", "nanoctl")
            .context("operating system has no configuration directory")?;
        Ok(dirs.config_dir().join("config.toml"))
    }

    pub fn load(path: &Path) -> Result<Self> {
        let value = fs::read_to_string(path)
            .with_context(|| format!("cannot read configuration at {}", path.display()))?;
        let config: Self = toml::from_str(&value).context("configuration is invalid")?;
        config.validate()?;
        Ok(config)
    }

    pub fn load_or_default(path: &Path) -> Result<Self> {
        if path.exists() {
            Self::load(path)
        } else {
            Ok(Self::default())
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let parent = path.parent().context("configuration path has no parent")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
        let temporary = path.with_extension("toml.tmp");
        fs::write(&temporary, toml::to_string_pretty(self)?)
            .with_context(|| format!("cannot write {}", temporary.display()))?;
        set_owner_only(&temporary)?;
        fs::rename(&temporary, path)
            .with_context(|| format!("cannot replace {}", path.display()))?;
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        if !matches!(self.control_plane_url.scheme(), "https" | "http") {
            bail!("control_plane_url must use https");
        }
        if self.control_plane_url.scheme() == "http"
            && !matches!(
                self.control_plane_url.host_str(),
                Some("127.0.0.1" | "localhost")
            )
        {
            bail!("plain HTTP is allowed only for localhost development");
        }
        if !matches!(self.quality.max_fps, 30 | 60 | 90 | 120) {
            bail!("quality.max_fps must be 30, 60, 90, or 120");
        }
        if !(500..=100_000).contains(&self.quality.max_bitrate_kbps) {
            bail!("quality.max_bitrate_kbps must be between 500 and 100000");
        }
        if !(320..=7680).contains(&self.quality.max_width)
            || !(240..=4320).contains(&self.quality.max_height)
        {
            bail!("quality resolution limits are outside the supported range");
        }
        if !(5..=300).contains(&self.network.heartbeat_seconds) {
            bail!("network.heartbeat_seconds must be between 5 and 300");
        }
        if !(250..=10_000).contains(&self.network.poll_milliseconds) {
            bail!("network.poll_milliseconds must be between 250 and 10000");
        }
        Ok(())
    }

    pub fn validate_enrolled(&self) -> Result<()> {
        self.validate()?;
        if self.device_id.as_deref().is_none_or(str::is_empty) {
            bail!("agent is not enrolled; run `nanoctl enroll CODE` first");
        }
        Ok(())
    }
}

#[cfg(unix)]
fn set_owner_only(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_owner_only(_path: &Path) -> Result<()> {
    // Windows installer applies an explicit service/user ACL to the configuration directory.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_configuration_keys() {
        let error = toml::from_str::<AgentConfig>(
            r#"
            control_plane_url = "https://example.com"
            unsafe_shell = true
            "#,
        )
        .expect_err("unknown key must fail");
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn rejects_insecure_remote_control_plane() {
        let config = AgentConfig {
            control_plane_url: Url::parse("http://example.com").unwrap(),
            ..AgentConfig::default()
        };
        assert!(config.validate().is_err());
    }
}
