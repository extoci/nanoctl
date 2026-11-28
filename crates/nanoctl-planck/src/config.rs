use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct PlanckConfig {
    pub labs: BTreeMap<String, bool>,
}

impl PlanckConfig {
    pub fn path() -> Result<PathBuf> {
        let dirs = ProjectDirs::from("dev", "nanoctl", "desktop")
            .context("operating system has no configuration directory")?;
        Ok(dirs.config_dir().join("config.toml"))
    }

    pub fn load_or_default(path: &std::path::Path) -> Self {
        let Ok(value) = fs::read_to_string(path) else {
            return Self::default();
        };
        toml::from_str(&value).unwrap_or_default()
    }

    pub fn save(&self, path: &std::path::Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        let value = toml::to_string_pretty(self).context("cannot serialize planck settings")?;
        let temporary = path.with_extension("toml.tmp");
        fs::write(&temporary, value)
            .with_context(|| format!("cannot write {}", temporary.display()))?;
        fs::rename(&temporary, path)
            .with_context(|| format!("cannot replace {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrupted_configuration_falls_back_to_defaults() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("planck.toml");
        fs::write(&path, "not valid [toml").unwrap();
        assert!(PlanckConfig::load_or_default(&path).labs.is_empty());
    }

    #[test]
    fn labs_round_trip() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("planck.toml");
        let mut config = PlanckConfig::default();
        config.labs.insert("experiment".to_owned(), true);
        config.save(&path).unwrap();
        assert!(PlanckConfig::load_or_default(&path).labs["experiment"]);
    }
}
