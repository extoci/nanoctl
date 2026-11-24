//! Always-on diagnostics log and the paste-ready `nanoctl debug` report built from it.

use std::{
    fmt::Write as _,
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use directories::ProjectDirs;
use tracing_subscriber::{
    EnvFilter, Layer,
    fmt::{
        format::Writer,
        time::{FormatTime, SystemTime},
    },
    registry::LookupSpan,
};

use crate::config::AgentConfig;

/// Rotate at this size, keeping one previous file: bounded disk use, hours of history.
const LOG_LIMIT: u64 = 2 * 1024 * 1024;
pub const REPORT_LOG_LINES: usize = 400;

pub fn log_path() -> Option<PathBuf> {
    ProjectDirs::from("dev", "nanoctl", "nanoctl").map(|project| {
        project
            .data_local_dir()
            .join("logs")
            .join("diagnostics.log")
    })
}

struct RotatingLog {
    path: PathBuf,
    state: Mutex<Option<(File, u64)>>,
}

impl RotatingLog {
    fn open(path: &Path) -> std::io::Result<(File, u64)> {
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let file = options.open(path)?;
        let size = file.metadata()?.len();
        Ok((file, size))
    }

    fn write(&self, bytes: &[u8]) -> std::io::Result<()> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if state.as_ref().is_none_or(|(_, size)| *size >= LOG_LIMIT) {
            if state.take().is_some() {
                let _ = std::fs::rename(&self.path, self.path.with_extension("log.1"));
            }
            *state = Some(Self::open(&self.path)?);
        }
        let (file, size) = state.as_mut().expect("log opened");
        file.write_all(bytes)?;
        *size += bytes.len() as u64;
        Ok(())
    }
}

struct LogWriter(&'static RotatingLog);

impl Write for LogWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.write(bytes)?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn private_directory(directory: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

static LOG: OnceLock<Option<RotatingLog>> = OnceLock::new();

fn rotating_log(path: PathBuf) -> Option<&'static RotatingLog> {
    LOG.get_or_init(|| {
        private_directory(path.parent()?).ok()?;
        let (file, size) = RotatingLog::open(&path).ok()?;
        Some(RotatingLog {
            path,
            state: Mutex::new(Some((file, size))),
        })
    })
    .as_ref()
}

/// Info-level agent events, independent of the console verbosity, so `nanoctl debug` and the
/// viewer's debug copy always have history. `RUST_LOG` raises or lowers the level.
pub fn layer<S>() -> Option<impl Layer<S>>
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    let log = rotating_log(log_path()?)?;
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("nanoctl=info"));
    Some(
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(move || LogWriter(log))
            .with_filter(filter),
    )
}

/// Appends a raw line (panics, fatal command errors) once the diagnostics log is open.
pub fn append(message: std::fmt::Arguments<'_>) {
    if let Some(log) = LOG.get().and_then(Option::as_ref) {
        let _ = log.write(format!("{} ERROR {message}\n", now()).as_bytes());
    }
}

/// The last `lines` lines across the rotated and current log files.
pub fn tail(path: &Path, lines: usize) -> Option<String> {
    let text = read_log(path)?;
    let all: Vec<&str> = text.lines().collect();
    Some(all[all.len().saturating_sub(lines)..].join("\n"))
}

fn read_log(path: &Path) -> Option<String> {
    let mut text = std::fs::read_to_string(path.with_extension("log.1")).unwrap_or_default();
    text.push_str(&std::fs::read_to_string(path).ok()?);
    Some(text)
}

const REPORT_WARNINGS: usize = 40;

/// Distinct WARN/ERROR lines from the whole log, so failures older than the tail survive:
/// `(count, last timestamp, line without timestamp)`, most recent last.
fn warnings(text: &str, limit: usize) -> Vec<(usize, &str, &str)> {
    let mut seen: Vec<(usize, &str, &str)> = Vec::new();
    for line in text.lines() {
        let Some((time, rest)) = line.split_once(' ') else {
            continue;
        };
        let rest = rest.trim_start();
        if !(rest.starts_with("WARN ") || rest.starts_with("ERROR ")) {
            continue;
        }
        if let Some(index) = seen.iter().position(|(_, _, known)| *known == rest) {
            let (count, _, _) = seen.remove(index);
            seen.push((count + 1, time, rest));
        } else {
            seen.push((1, time, rest));
        }
    }
    let skip = seen.len().saturating_sub(limit);
    seen.split_off(skip)
}

fn os_version() -> String {
    fn output(program: &str, arguments: &[&str]) -> Option<String> {
        let mut command = std::process::Command::new(program);
        command.args(arguments).stdin(std::process::Stdio::null());
        #[cfg(windows)]
        std::os::windows::process::CommandExt::creation_flags(&mut command, 0x0800_0000);
        let output = command.output().ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
            .filter(|text| !text.is_empty())
    }
    #[cfg(target_os = "linux")]
    let name = std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("PRETTY_NAME="))
                .map(|name| name.trim_matches('"').to_owned())
        });
    #[cfg(target_os = "macos")]
    let name = output("sw_vers", &["-productVersion"]).map(|version| format!("macOS {version}"));
    #[cfg(windows)]
    let name = output("cmd", &["/C", "ver"]);
    #[cfg(unix)]
    let kernel = output("uname", &["-sr"]);
    #[cfg(not(unix))]
    let kernel: Option<String> = None;
    match (name, kernel) {
        (Some(name), Some(kernel)) => format!("{name} ({kernel})"),
        (Some(name), None) | (None, Some(name)) => name,
        (None, None) => "unknown".into(),
    }
}

fn features() -> String {
    let features: Vec<&str> = [
        ("media", cfg!(feature = "media")),
        ("rtc", cfg!(feature = "rtc")),
        ("x264", cfg!(feature = "x264")),
    ]
    .into_iter()
    .filter_map(|(name, enabled)| enabled.then_some(name))
    .collect();
    if features.is_empty() {
        "none".into()
    } else {
        features.join(", ")
    }
}

pub fn now() -> String {
    let mut time = String::new();
    let _ = SystemTime.format_time(&mut Writer::new(&mut time));
    time
}

/// A Markdown report a user can paste to a person or coding agent: build, OS, readiness,
/// encoder/codec/display capabilities, redacted configuration, and recent agent logs.
pub async fn report(config: &AgentConfig, config_path: Option<&Path>, lines: usize) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# nanoctl debug report\n");
    let _ = writeln!(
        out,
        "Generated {}. Contains tailnet names, IPs and logins; review before sharing publicly.\n",
        now()
    );
    let _ = writeln!(out, "## Host\n");
    let _ = writeln!(out, "- version: {}", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(
        out,
        "- target: {}-{}",
        crate::platform::PLATFORM,
        crate::platform::ARCHITECTURE
    );
    let _ = writeln!(out, "- os: {}", os_version());
    let _ = writeln!(out, "- features: {}", features());
    #[cfg(feature = "media")]
    let _ = writeln!(
        out,
        "- max fps: {} (configured {})",
        crate::media::effective_max_fps(config.quality.max_fps),
        config.quality.max_fps
    );

    let doctor = crate::platform::doctor(config).await;
    let _ = writeln!(
        out,
        "\n## Doctor ({})\n",
        if doctor.ready { "ready" } else { "not ready" }
    );
    for check in &doctor.checks {
        let _ = writeln!(
            out,
            "- {} {}: {}",
            if check.ok { "ok" } else { "FAIL" },
            check.name,
            check.detail
        );
    }

    let capabilities = crate::platform::capabilities(
        config.features.remote_input,
        config.features.clipboard && config.features.remote_input,
        config.features.system_audio,
        config.quality.encoder,
    );
    let _ = writeln!(
        out,
        "\n## Capabilities\n\n```json\n{}\n```",
        serde_json::to_string_pretty(&capabilities).unwrap_or_default()
    );

    let _ = writeln!(out, "\n## Configuration\n");
    if let Some(path) = config_path {
        let _ = writeln!(out, "Path: `{}`\n", path.display());
    }
    let _ = writeln!(
        out,
        "```toml\n{}```",
        toml::to_string_pretty(config).unwrap_or_default()
    );

    let log = log_path().and_then(|path| read_log(&path));
    if let Some(text) = &log {
        let distinct = warnings(text, REPORT_WARNINGS);
        let _ = writeln!(out, "\n## Warnings and errors (whole log)\n");
        if distinct.is_empty() {
            let _ = writeln!(out, "None.");
        } else {
            let _ = writeln!(out, "```text");
            for (count, time, line) in distinct {
                let _ = writeln!(out, "{count}x last {time} {line}");
            }
            let _ = writeln!(out, "```");
        }
    }

    let _ = writeln!(out, "\n## Recent agent log\n");
    match log_path() {
        Some(path) => match log.as_deref().and(tail(&path, lines)) {
            Some(log) => {
                let _ = writeln!(out, "Last {lines} lines of `{}`:\n", path.display());
                let _ = writeln!(out, "```text\n{log}\n```");
            }
            None => {
                let _ = writeln!(
                    out,
                    "No log at `{}` yet; the agent writes it while `nanoctl run` is active.",
                    path.display()
                );
            }
        },
        None => {
            let _ = writeln!(out, "This OS has no local data directory for logs.");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_and_tails_across_files() {
        let dir = std::env::temp_dir().join(format!("nanoctl-diag-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("diagnostics.log");
        let log = RotatingLog {
            path: path.clone(),
            state: Mutex::new(None),
        };
        let line = format!("{}\n", "x".repeat(1023));
        for _ in 0..(LOG_LIMIT / 1024) {
            log.write(line.as_bytes()).unwrap();
        }
        log.write(b"after rotation\n").unwrap();
        assert!(path.with_extension("log.1").exists());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "after rotation\n");
        let tail = tail(&path, 2).unwrap();
        assert_eq!(tail, format!("{}\nafter rotation", "x".repeat(1023)));
        assert!(super::tail(&dir.join("missing.log"), 5).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn warnings_are_distinct_counted_and_most_recent_last() {
        let text = "\
t1  WARN nanoctl::media: hardware encode failed
t2  INFO nanoctl::rtc: negotiated video codec
t3 ERROR nanoctl: panicked at src/media.rs
t4  WARN nanoctl::media: hardware encode failed
garbage";
        assert_eq!(
            warnings(text, 10),
            vec![
                (1, "t3", "ERROR nanoctl: panicked at src/media.rs"),
                (2, "t4", "WARN nanoctl::media: hardware encode failed"),
            ]
        );
        assert_eq!(warnings(text, 1).len(), 1);
    }

    #[tokio::test]
    async fn report_has_every_section() {
        let report = report(&AgentConfig::default(), Some(Path::new("/tmp/c.toml")), 10).await;
        for section in [
            "# nanoctl debug report",
            "## Host",
            "- version: ",
            "## Doctor",
            "## Capabilities",
            "## Configuration",
            "Path: `/tmp/c.toml`",
            "## Recent agent log",
        ] {
            assert!(report.contains(section), "missing {section}:\n{report}");
        }
    }
}
