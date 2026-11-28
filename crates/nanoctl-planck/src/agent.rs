use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use directories::ProjectDirs;

#[derive(Clone, Debug)]
pub struct AgentSnapshot {
    pub installed: bool,
    pub running: bool,
    pub version: Option<String>,
    pub binary_path: Option<PathBuf>,
    pub config_path: PathBuf,
    pub labs: BTreeMap<String, bool>,
}

pub fn config_path() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("dev", "nanoctl", "nanoctl")
        .context("operating system has no configuration directory")?;
    Ok(dirs.config_dir().join("config.toml"))
}

fn install_root() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|root| PathBuf::from(root).join("nanoctl"))
}

fn binary_path() -> Option<PathBuf> {
    if let Some(path) = install_root().map(|root| root.join("nanoctl.exe"))
        && path.is_file()
    {
        return Some(path);
    }
    if let Some(path) = std::env::var_os("ProgramFiles")
        .map(|root| PathBuf::from(root).join("nanoctl").join("nanoctl.exe"))
        && path.is_file()
    {
        return Some(path);
    }
    which_binary()
}

fn which_binary() -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    for root in std::env::split_paths(&paths) {
        for name in ["nanoctl.exe", "nanoctl"] {
            let path = root.join(name);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

fn service_lock_path() -> Option<PathBuf> {
    ProjectDirs::from("dev", "nanoctl", "nanoctl").map(|dirs| {
        dirs.data_local_dir()
            .join("service-control")
            .join("agent.lock")
    })
}

fn service_lock_held() -> bool {
    let Some(path) = service_lock_path() else {
        return false;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(file) = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
    else {
        return false;
    };
    matches!(file.try_lock(), Err(std::fs::TryLockError::WouldBlock))
}

fn read_labs(path: &Path) -> BTreeMap<String, bool> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    let Ok(value) = toml::from_str::<toml::Value>(&text) else {
        return BTreeMap::new();
    };
    value
        .get("labs")
        .and_then(toml::Value::as_table)
        .map(|table| {
            table
                .iter()
                .filter_map(|(key, value)| value.as_bool().map(|enabled| (key.clone(), enabled)))
                .collect()
        })
        .unwrap_or_default()
}

pub fn inspect() -> AgentSnapshot {
    let config_path = config_path().unwrap_or_else(|_| PathBuf::from("config.toml"));
    let binary_path = binary_path();
    let version = binary_path.as_ref().and_then(|path| {
        Command::new(path)
            .arg("--version")
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    });
    let running = service_lock_held();
    AgentSnapshot {
        installed: binary_path.is_some(),
        running,
        version,
        binary_path,
        labs: read_labs(&config_path),
        config_path,
    }
}

pub fn set_host_lab(id: &str, enabled: bool) -> Result<()> {
    set_host_lab_in_path(config_path()?, id, enabled)
}

fn set_host_lab_in_path(path: PathBuf, id: &str, enabled: bool) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut value = if path.exists() {
        toml::from_str::<toml::Value>(
            &std::fs::read_to_string(&path)
                .with_context(|| format!("cannot read {}", path.display()))?,
        )
        .context("agent configuration is invalid")?
    } else {
        toml::Value::Table(toml::map::Map::new())
    };
    let root = value
        .as_table_mut()
        .context("agent configuration is invalid")?;
    let labs = root
        .entry("labs")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    labs.as_table_mut()
        .context("agent labs configuration must be a table")?
        .insert(id.to_owned(), toml::Value::Boolean(enabled));
    let rendered = toml::to_string_pretty(&value)?;
    let temporary = path.with_extension("toml.tmp");
    std::fs::write(&temporary, rendered)?;
    std::fs::rename(&temporary, &path)?;
    Ok(())
}

fn run(binary: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new(binary)
        .args(args)
        .output()
        .with_context(|| format!("cannot run {}", binary.display()))?;
    let text = String::from_utf8_lossy(if output.stderr.is_empty() {
        &output.stdout
    } else {
        &output.stderr
    })
    .trim()
    .to_owned();
    if !output.status.success() {
        bail!(if text.is_empty() {
            "agent command failed".to_owned()
        } else {
            text
        });
    }
    Ok(if text.is_empty() {
        "Done".to_owned()
    } else {
        text
    })
}

fn scheduled_task_exists() -> bool {
    Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "if (Get-ScheduledTask -TaskName 'nanoctl Agent' -ErrorAction SilentlyContinue) { exit 0 } else { exit 1 }",
        ])
        .output()
        .is_ok_and(|output| output.status.success())
}

fn start_scheduled_task() -> Result<()> {
    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Start-ScheduledTask -TaskName 'nanoctl Agent'",
        ])
        .output()
        .context("cannot start the nanoctl scheduled task")?;
    if output.status.success() {
        Ok(())
    } else {
        bail!(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

pub fn start() -> Result<String> {
    let binary = binary_path().context("nanoctl agent is not installed")?;
    if scheduled_task_exists() {
        start_scheduled_task()?;
        Ok("Agent start requested".to_owned())
    } else {
        let mut command = Command::new(&binary);
        if let Some(root) = install_root() {
            std::fs::create_dir_all(&root).context("cannot create nanoctl data directory")?;
            command.arg("--ready-file").arg(root.join("agent.ready"));
        }
        command
            .arg("run")
            .spawn()
            .context("cannot start nanoctl agent")?;
        Ok("Agent starting".to_owned())
    }
}

pub fn stop() -> Result<String> {
    let binary = binary_path().context("nanoctl agent is not installed")?;
    run(&binary, &["stop"])
}

pub fn restart() -> Result<String> {
    let binary = binary_path().context("nanoctl agent is not installed")?;
    let _ = run(&binary, &["stop"]);
    start()
}

pub fn open_file(path: &Path) -> Result<()> {
    Command::new("explorer")
        .arg(path)
        .spawn()
        .context("cannot open path")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_labs_round_trip_preserves_other_config() {
        let root =
            std::env::temp_dir().join(format!("nanoctl-planck-agent-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("config.toml");
        std::fs::write(
            &path,
            "[quality]
mode = \"balanced\"

[labs]
\"future.host-feature\" = false
",
        )
        .unwrap();

        set_host_lab_in_path(path.clone(), "windows.strict-dxgi-capture", true).unwrap();
        set_host_lab_in_path(path.clone(), "windows.strict-dxgi-capture", false).unwrap();

        let labs = read_labs(&path);
        assert!(!labs["windows.strict-dxgi-capture"]);
        assert!(!labs["future.host-feature"]);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[quality]"));
        assert!(text.contains("mode = \"balanced\""));
        assert!(text.contains("future.host-feature"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
