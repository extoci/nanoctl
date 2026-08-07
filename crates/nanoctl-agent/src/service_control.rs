use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use directories::ProjectDirs;
use tokio::time::Instant;

const STOP_TIMEOUT: Duration = Duration::from_secs(30);
const STOP_QUIET_WINDOW: Duration = Duration::from_millis(500);

#[derive(Clone, Debug)]
struct ServiceControlPaths {
    directory: PathBuf,
    service_lock: PathBuf,
    command_lock: PathBuf,
}

impl ServiceControlPaths {
    fn default() -> Result<Self> {
        let project = ProjectDirs::from("dev", "nanoctl", "nanoctl")
            .context("operating system has no local data directory")?;
        Ok(Self::in_directory(
            project.data_local_dir().join("service-control"),
        ))
    }

    fn in_directory(directory: PathBuf) -> Self {
        Self {
            service_lock: directory.join("agent.lock"),
            command_lock: directory.join("stop-command.lock"),
            directory,
        }
    }

    fn prepare(&self) -> Result<()> {
        std::fs::create_dir_all(&self.directory).with_context(|| {
            format!(
                "cannot create service control directory {}",
                self.directory.display()
            )
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.directory, std::fs::Permissions::from_mode(0o700))
                .with_context(|| {
                    format!(
                        "cannot protect service control directory {}",
                        self.directory.display()
                    )
                })?;
        }
        Ok(())
    }

    fn open_lock(&self, path: &Path) -> Result<File> {
        self.prepare()?;
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .with_context(|| format!("cannot open service control lock {}", path.display()))
    }

    fn create_stop_request(&self, timeout: Duration) -> Result<StopRequest> {
        self.prepare()?;
        let now = unix_millis();
        let deadline_millis = now.saturating_add(timeout.as_millis() as u64);
        let nonce = format!("{}-{}", std::process::id(), unix_nanos());
        let path = self
            .directory
            .join(format!("stop-{deadline_millis}-{nonce}.request"));
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| format!("cannot publish stop request {}", path.display()))?;
        Ok(StopRequest {
            acknowledgement: path.with_extension("ack"),
            path,
            deadline_millis,
        })
    }

    fn current_stop_requests(&self) -> Result<Vec<StopRequest>> {
        self.prepare()?;
        let now = unix_millis();
        let mut current = Vec::new();
        for entry in std::fs::read_dir(&self.directory).with_context(|| {
            format!(
                "cannot inspect service control directory {}",
                self.directory.display()
            )
        })? {
            let entry = entry.context("cannot inspect service control entry")?;
            let path = entry.path();
            let Some(deadline_millis) = stop_request_deadline(&path) else {
                continue;
            };
            let request = StopRequest {
                acknowledgement: path.with_extension("ack"),
                path,
                deadline_millis,
            };
            if deadline_millis >= now {
                current.push(request);
            } else {
                request.remove()?;
            }
        }
        Ok(current)
    }
}

#[derive(Debug)]
struct StopRequest {
    path: PathBuf,
    acknowledgement: PathBuf,
    deadline_millis: u64,
}

impl StopRequest {
    fn acknowledge(&self) -> Result<()> {
        OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&self.acknowledgement)
            .with_context(|| {
                format!(
                    "cannot acknowledge service stop at {}",
                    self.acknowledgement.display()
                )
            })?;
        Ok(())
    }

    fn acknowledged(&self) -> bool {
        self.acknowledgement.is_file()
    }

    fn remove(&self) -> Result<()> {
        remove_if_present(&self.acknowledgement)?;
        remove_if_present(&self.path)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LockState {
    Acquired,
    Held,
}

fn try_lock(lock: &File) -> std::io::Result<LockState> {
    match lock.try_lock() {
        Ok(()) => Ok(LockState::Acquired),
        Err(std::fs::TryLockError::WouldBlock) => Ok(LockState::Held),
        Err(std::fs::TryLockError::Error(error)) => Err(error),
    }
}

pub(crate) struct ServiceControl {
    _service_lock: File,
    paths: ServiceControlPaths,
}

impl ServiceControl {
    /// Acquires the singleton service lock. `None` means a current stop transaction was already
    /// published; startup acknowledged it and must exit successfully without becoming ready.
    pub(crate) fn acquire() -> Result<Option<Self>> {
        Self::acquire_with_paths(ServiceControlPaths::default()?)
    }

    fn acquire_with_paths(paths: ServiceControlPaths) -> Result<Option<Self>> {
        let service_lock = paths.open_lock(&paths.service_lock)?;
        if try_lock(&service_lock).context("cannot lock the nanoctl background agent")?
            == LockState::Held
        {
            bail!("the nanoctl background agent is already running")
        }
        let stop_requests = paths.current_stop_requests()?;
        if !stop_requests.is_empty() {
            for request in stop_requests {
                request.acknowledge()?;
            }
            return Ok(None);
        }
        Ok(Some(Self {
            _service_lock: service_lock,
            paths,
        }))
    }

    pub(crate) fn take_stop_request(&self) -> Result<bool> {
        let stop_requests = self.paths.current_stop_requests()?;
        if stop_requests.is_empty() {
            return Ok(false);
        }
        for request in stop_requests {
            request.acknowledge()?;
        }
        Ok(true)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StopOutcome {
    Stopped,
    AlreadyStopped,
}

pub(crate) async fn request_stop() -> Result<StopOutcome> {
    request_stop_with_paths(
        ServiceControlPaths::default()?,
        STOP_TIMEOUT,
        stop_legacy_service,
    )
    .await
}

async fn request_stop_with_paths<F>(
    paths: ServiceControlPaths,
    timeout: Duration,
    legacy_stop: F,
) -> Result<StopOutcome>
where
    F: FnOnce() -> Result<LegacyStopOutcome>,
{
    let command_lock = paths.open_lock(&paths.command_lock)?;
    let command_deadline = Instant::now() + timeout;
    while try_lock(&command_lock).context("cannot serialize nanoctl stop commands")?
        == LockState::Held
    {
        if Instant::now() >= command_deadline {
            bail!("timed out waiting for another nanoctl stop command")
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let request = paths.create_stop_request(timeout)?;
    let initially_running = service_is_running(&paths)?;
    let legacy_outcome = if initially_running {
        LegacyStopOutcome::NotNeeded
    } else {
        legacy_stop()?
    };
    let mut observed_running = initially_running || legacy_outcome == LegacyStopOutcome::Stopped;
    let mut quiet_since = None;

    loop {
        let running = service_is_running(&paths)?;
        observed_running |= running || request.acknowledged();
        if running {
            quiet_since = None;
        } else {
            let quiet_started = quiet_since.get_or_insert_with(Instant::now);
            if Instant::now().duration_since(*quiet_started) >= STOP_QUIET_WINDOW {
                request.remove()?;
                return Ok(if observed_running {
                    StopOutcome::Stopped
                } else {
                    StopOutcome::AlreadyStopped
                });
            }
        }
        if Instant::now() >= command_deadline || unix_millis() > request.deadline_millis {
            bail!("timed out waiting for the nanoctl background agent to stop")
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn service_is_running(paths: &ServiceControlPaths) -> Result<bool> {
    let service_lock = paths.open_lock(&paths.service_lock)?;
    Ok(
        try_lock(&service_lock).context("cannot inspect the nanoctl background agent")?
            == LockState::Held,
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LegacyStopOutcome {
    Stopped,
    AlreadyStopped,
    NotNeeded,
}

#[cfg(target_os = "windows")]
fn stop_legacy_service() -> Result<LegacyStopOutcome> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    match windows_legacy_task_state(CREATE_NO_WINDOW)? {
        WindowsLegacyTaskState::Missing | WindowsLegacyTaskState::Stopped => {
            return Ok(LegacyStopOutcome::AlreadyStopped);
        }
        WindowsLegacyTaskState::Running => {}
    }
    let mut end = Command::new("schtasks.exe");
    end.args(["/End", "/TN", "nanoctl Agent"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
    if !end
        .status()
        .context("cannot stop the legacy nanoctl Scheduled Task")?
        .success()
    {
        bail!("Windows Task Scheduler rejected the legacy nanoctl stop request")
    }

    Ok(LegacyStopOutcome::Stopped)
}

#[cfg(target_os = "windows")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WindowsLegacyTaskState {
    Missing,
    Stopped,
    Running,
}

#[cfg(target_os = "windows")]
fn windows_legacy_task_state(create_no_window: u32) -> Result<WindowsLegacyTaskState> {
    use std::os::windows::process::CommandExt;

    // Numeric ScheduledTaskState values avoid localized `schtasks.exe /Query` output. Enumerating
    // tasks also distinguishes a genuinely absent task from access/query failures, which must not
    // be reported as a successful idempotent stop.
    const QUERY: &str = "$ErrorActionPreference='Stop'; try { $tasks=@(Get-ScheduledTask -ErrorAction Stop | Where-Object { $_.TaskName -eq 'nanoctl Agent' -and $_.TaskPath -eq '\\' }); if ($tasks.Count -eq 0) { exit 3 }; if (@($tasks | Where-Object { [int]$_.State -eq 4 }).Count -gt 0) { exit 4 }; exit 0 } catch { exit 2 }";
    let mut query = Command::new("powershell.exe");
    query
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            QUERY,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(create_no_window);
    let status = query
        .status()
        .context("cannot inspect the legacy nanoctl Scheduled Task")?;
    match status.code() {
        Some(0) => Ok(WindowsLegacyTaskState::Stopped),
        Some(3) => Ok(WindowsLegacyTaskState::Missing),
        Some(4) => Ok(WindowsLegacyTaskState::Running),
        _ => bail!("Windows Task Scheduler could not inspect the legacy nanoctl task"),
    }
}

#[cfg(target_os = "linux")]
fn stop_legacy_service() -> Result<LegacyStopOutcome> {
    let active = quiet_command(
        "systemctl",
        &["--user", "is-active", "--quiet", "nanoctl.service"],
    )
    .context("cannot query the legacy nanoctl systemd user service")?;
    if !active {
        return Ok(LegacyStopOutcome::AlreadyStopped);
    }
    if !quiet_command("systemctl", &["--user", "stop", "nanoctl.service"])
        .context("cannot stop the legacy nanoctl systemd user service")?
    {
        bail!("systemd did not stop the legacy nanoctl user service")
    }
    Ok(LegacyStopOutcome::Stopped)
}

#[cfg(target_os = "macos")]
fn stop_legacy_service() -> Result<LegacyStopOutcome> {
    // UID is a shell variable in zsh/bash but is not normally exported to child processes.
    // Resolve it through the platform utility instead of relying on the caller's environment.
    let uid_output = Command::new("id")
        .arg("-u")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .context("cannot resolve the current user for launchd service control")?;
    if !uid_output.status.success() {
        bail!("could not resolve the current user for launchd service control")
    }
    let uid = String::from_utf8(uid_output.stdout)
        .context("current launchd user id was not valid UTF-8")?;
    let uid = uid.trim();
    if uid.is_empty() || !uid.bytes().all(|byte| byte.is_ascii_digit()) {
        bail!("current launchd user id was invalid")
    }
    let target = format!("gui/{uid}/dev.nanoctl.agent");
    if !quiet_command("launchctl", &["print", &target])
        .context("cannot query the legacy nanoctl LaunchAgent")?
    {
        return Ok(LegacyStopOutcome::AlreadyStopped);
    }
    if !quiet_command("launchctl", &["bootout", &target])
        .context("cannot stop the legacy nanoctl LaunchAgent")?
    {
        bail!("launchd did not stop the legacy nanoctl agent")
    }
    Ok(LegacyStopOutcome::Stopped)
}

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
fn stop_legacy_service() -> Result<LegacyStopOutcome> {
    Ok(LegacyStopOutcome::AlreadyStopped)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn quiet_command(program: &str, arguments: &[&str]) -> std::io::Result<bool> {
    Ok(Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?
        .success())
}

fn stop_request_deadline(path: &Path) -> Option<u64> {
    let name = path.file_name()?.to_str()?;
    let remainder = name.strip_prefix("stop-")?.strip_suffix(".request")?;
    let (deadline, _nonce) = remainder.split_once('-')?;
    deadline.parse().ok()
}

fn remove_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("cannot remove {}", path.display())),
    }
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

fn unix_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

#[cfg(test)]
mod tests {
    use super::{
        LegacyStopOutcome, ServiceControl, ServiceControlPaths, StopOutcome,
        request_stop_with_paths,
    };
    use std::time::Duration;

    #[tokio::test]
    async fn stop_command_requests_shutdown_and_waits_for_service_exit() {
        let temporary = tempfile::tempdir().expect("temporary service directory");
        let paths = ServiceControlPaths::in_directory(temporary.path().to_owned());
        let control = ServiceControl::acquire_with_paths(paths.clone())
            .expect("running service lock")
            .expect("service should start");
        let stop = tokio::spawn(request_stop_with_paths(
            paths.clone(),
            Duration::from_secs(2),
            || Ok(LegacyStopOutcome::NotNeeded),
        ));

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if control.take_stop_request().expect("read stop request") {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("stop request should arrive");
        drop(control);

        assert_eq!(
            stop.await
                .expect("stop task should finish")
                .expect("stop request should succeed"),
            StopOutcome::Stopped
        );
    }

    #[tokio::test]
    async fn stop_command_is_idempotent_when_service_is_not_running() {
        let temporary = tempfile::tempdir().expect("temporary service directory");
        let paths = ServiceControlPaths::in_directory(temporary.path().to_owned());

        assert_eq!(
            request_stop_with_paths(paths, Duration::from_secs(2), || {
                Ok(LegacyStopOutcome::AlreadyStopped)
            })
            .await
            .expect("idempotent stop"),
            StopOutcome::AlreadyStopped
        );
    }

    #[tokio::test]
    async fn stop_command_reports_a_stopped_legacy_service_without_a_new_lock() {
        let temporary = tempfile::tempdir().expect("temporary service directory");
        let paths = ServiceControlPaths::in_directory(temporary.path().to_owned());

        assert_eq!(
            request_stop_with_paths(paths, Duration::from_secs(2), || {
                Ok(LegacyStopOutcome::Stopped)
            })
            .await
            .expect("legacy stop"),
            StopOutcome::Stopped
        );
    }

    #[test]
    fn service_start_honors_an_in_flight_stop_transaction() {
        let temporary = tempfile::tempdir().expect("temporary service directory");
        let paths = ServiceControlPaths::in_directory(temporary.path().to_owned());
        let request = paths
            .create_stop_request(Duration::from_secs(2))
            .expect("stop request");

        assert!(
            ServiceControl::acquire_with_paths(paths)
                .expect("service start check")
                .is_none()
        );
        assert!(request.acknowledged());
    }

    #[test]
    fn a_second_service_instance_cannot_acquire_the_agent_lock() {
        let temporary = tempfile::tempdir().expect("temporary service directory");
        let paths = ServiceControlPaths::in_directory(temporary.path().to_owned());
        let _control = ServiceControl::acquire_with_paths(paths.clone())
            .expect("first service lock")
            .expect("service should start");

        let error = ServiceControl::acquire_with_paths(paths)
            .err()
            .expect("duplicate service must fail");
        assert!(error.to_string().contains("already running"));
    }
}
