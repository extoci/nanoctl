use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct AgentConfig {
    pub tailnet: TailnetConfig,
    pub quality: QualityConfig,
    pub features: FeatureConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct QualityConfig {
    pub codec: CodecPreference,
    pub encoder: EncoderPreference,
    pub max_fps: u16,
    pub max_bitrate_kbps: u32,
    pub max_width: u32,
    pub max_height: u32,
    pub latency_mode: LatencyMode,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct TailnetConfig {
    pub discover_peers: bool,
    pub port: u16,
    pub allow_users: Vec<String>,
    pub allow_tags: Vec<String>,
    /// Only enable on single-user machines when fronted by tailscale serve.
    pub serve_loopback: bool,
    pub tls: TlsMode,
    pub tailscale_cli: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct FeatureConfig {
    pub system_audio: bool,
    pub clipboard: bool,
    pub remote_input: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TlsMode {
    #[default]
    Auto,
    Off,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CodecPreference {
    #[default]
    Auto,
    H264,
    Hevc,
    Vp9,
    Av1,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EncoderPreference {
    #[default]
    Auto,
    Hardware,
    Software,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LatencyMode {
    Responsiveness,
    #[default]
    Balanced,
    Quality,
}

impl Default for QualityConfig {
    fn default() -> Self {
        Self {
            codec: CodecPreference::Auto,
            encoder: EncoderPreference::Auto,
            max_fps: 0,
            max_bitrate_kbps: 50_000,
            max_width: 3840,
            max_height: 2160,
            latency_mode: LatencyMode::Balanced,
        }
    }
}

impl Default for TailnetConfig {
    fn default() -> Self {
        Self {
            discover_peers: true,
            port: 7337,
            allow_users: Vec::new(),
            allow_tags: Vec::new(),
            serve_loopback: false,
            tls: TlsMode::Auto,
            tailscale_cli: None,
        }
    }
}

impl Default for FeatureConfig {
    fn default() -> Self {
        Self {
            system_audio: true,
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
        let mut raw: toml::Value = toml::from_str(&value).context("configuration is invalid")?;
        drop_control_plane_defaults(&mut raw);
        let mut known = Self::default();
        known.tailnet.tailscale_cli = Some(PathBuf::new());
        warn_unknown(&raw, &toml::Value::try_from(known)?, "");
        let config: Self = raw.try_into().context("configuration is invalid")?;
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

    pub fn validate(&self) -> Result<()> {
        if self.tailnet.port == 0 {
            bail!("tailnet.port must not be zero");
        }
        if self.quality.max_fps != 0 && !(24..=240).contains(&self.quality.max_fps) {
            bail!("quality.max_fps must be 0 (display refresh rate) or between 24 and 240");
        }
        if !(500..=100_000).contains(&self.quality.max_bitrate_kbps) {
            bail!("quality.max_bitrate_kbps must be between 500 and 100000");
        }
        if !(320..=7680).contains(&self.quality.max_width)
            || !(240..=4320).contains(&self.quality.max_height)
        {
            bail!("quality resolution limits are outside the supported range");
        }
        if matches!(self.quality.codec, CodecPreference::Vp9) {
            bail!("quality.codec must be auto, h264, hevc or av1");
        }
        Ok(())
    }
}

/// Control-plane releases saved every default to disk, so their `[quality]` and `[features]`
/// tables pin values that have since changed (audio and clipboard off, 60 fps, 24 Mbps).
/// Those values are indistinguishable from a choice, so files with the control-plane marker
/// fall back to current defaults for them; any other value is kept as configured.
const CONTROL_PLANE_DEFAULTS: [(&str, &str, toml::Value); 4] = [
    ("quality", "max_fps", toml::Value::Integer(60)),
    ("quality", "max_bitrate_kbps", toml::Value::Integer(24_000)),
    ("features", "system_audio", toml::Value::Boolean(false)),
    ("features", "clipboard", toml::Value::Boolean(false)),
];

fn drop_control_plane_defaults(raw: &mut toml::Value) {
    let Some(root) = raw.as_table_mut() else {
        return;
    };
    if !root.contains_key("control_plane_url") {
        return;
    }
    for (section, key, legacy) in &CONTROL_PLANE_DEFAULTS {
        let Some(table) = root.get_mut(*section).and_then(toml::Value::as_table_mut) else {
            continue;
        };
        if table.get(*key) == Some(legacy) {
            table.remove(*key);
            tracing::warn!(
                key = format!("{section}.{key}"),
                "using the current default instead of a value saved by a control-plane release; remove control_plane_url to keep it"
            );
        }
    }
}

fn warn_unknown(value: &toml::Value, known: &toml::Value, prefix: &str) {
    if let Some(table) = value.as_table() {
        for (key, value) in table {
            let path = format!("{prefix}{key}");
            match known.get(key) {
                Some(known) => warn_unknown(value, known, &format!("{path}.")),
                None => tracing::warn!(key = path, "ignoring unknown or legacy configuration key"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_configuration_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "device_id = 'old'\ncontrol_plane_url = 'https://old.example'\n[network]\nheartbeat_seconds = 30\n").unwrap();
        assert_eq!(AgentConfig::load(&path).unwrap().tailnet.port, 7337);
        assert!(AgentConfig::load(&path).unwrap().tailnet.discover_peers);
        fs::write(&path, "[tailnet]\ndiscover_peers = false\n").unwrap();
        assert!(!AgentConfig::load(&path).unwrap().tailnet.discover_peers);
    }

    #[test]
    fn control_plane_defaults_fall_back_to_current_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let saved = [
            "device_id = 'old'",
            "control_plane_url = 'https://old.example'",
            "[quality]",
            "max_fps = 60",
            "max_bitrate_kbps = 24000",
            "max_width = 1920",
            "[features]",
            "system_audio = false",
            "clipboard = false",
            "remote_input = false",
        ]
        .join("\n");
        fs::write(&path, &saved).unwrap();
        let config = AgentConfig::load(&path).unwrap();
        let defaults = AgentConfig::default();
        assert_eq!(config.quality.max_fps, defaults.quality.max_fps);
        assert_eq!(
            config.quality.max_bitrate_kbps,
            defaults.quality.max_bitrate_kbps
        );
        assert_eq!(config.quality.max_width, 1920);
        assert!(config.features.system_audio);
        assert!(config.features.clipboard);
        assert!(!config.features.remote_input);

        fs::write(&path, saved.replace("max_fps = 60", "max_fps = 120")).unwrap();
        assert_eq!(AgentConfig::load(&path).unwrap().quality.max_fps, 120);

        fs::write(
            &path,
            "[quality]\nmax_fps = 60\n[features]\nsystem_audio = false\n",
        )
        .unwrap();
        let config = AgentConfig::load(&path).unwrap();
        assert_eq!(config.quality.max_fps, 60);
        assert!(!config.features.system_audio);
    }

    #[test]
    fn accepts_clipboard_and_system_audio() {
        let mut config = AgentConfig::default();
        config.features.clipboard = true;
        assert!(config.validate().is_ok());
        config.features.clipboard = false;
        config.features.system_audio = true;
        assert!(config.validate().is_ok());
        assert!(AgentConfig::default().features.system_audio);
    }

    #[test]
    fn accepts_explicit_encoder_policy() {
        let mut config = AgentConfig::default();
        config.quality.encoder = EncoderPreference::Software;
        assert!(config.validate().is_ok());
        config.quality.encoder = EncoderPreference::Hardware;
        assert!(config.validate().is_ok());
    }
}
