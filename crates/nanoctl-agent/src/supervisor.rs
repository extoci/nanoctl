use std::{future::Future, io::Read, process::Stdio, time::Duration};

use anyhow::{Context, Result};
use tokio::{
    process::Command,
    sync::oneshot,
    time::{Instant, sleep, timeout},
};

const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
const STABLE_RUN: Duration = Duration::from_secs(60);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);

#[cfg(windows)]
pub(crate) async fn run(control: crate::service_control::ServiceControl) -> Result<()> {
    let mut interrupt = tokio::signal::windows::ctrl_c().context("cannot listen for Ctrl+C")?;
    let mut close =
        tokio::signal::windows::ctrl_close().context("cannot listen for console close")?;
    let mut command = Command::new(std::env::current_exe().context("cannot locate the agent")?);
    command
        .arg("--supervised-worker")
        .args(std::env::args_os().skip(1));
    supervise(command, async {
        tokio::select! {
            _ = interrupt.recv() => Ok("Ctrl+C"),
            _ = close.recv() => Ok("console close"),
            result = crate::service::wait_for_stop_request(&control) => result,
        }
    })
    .await
}

pub(crate) fn parent_shutdown() -> Result<oneshot::Receiver<()>> {
    let (shutdown, receiver) = oneshot::channel();
    std::thread::Builder::new()
        .name("supervisor-shutdown".into())
        .spawn(move || {
            let _ = std::io::stdin().lock().read(&mut [0]);
            let _ = shutdown.send(());
        })
        .context("cannot watch the supervisor shutdown pipe")?;
    Ok(receiver)
}

async fn supervise(
    mut command: Command,
    stop: impl Future<Output = Result<&'static str>>,
) -> Result<()> {
    command.stdin(Stdio::piped()).kill_on_drop(true);
    tokio::pin!(stop);
    let mut backoff = INITIAL_BACKOFF;
    loop {
        let mut worker = command.spawn().context("cannot start the agent worker")?;
        let pipe = worker.stdin.take().context("missing agent shutdown pipe")?;
        let started = Instant::now();
        let status = tokio::select! {
            biased;
            result = &mut stop => {
                let reason = result.as_ref().copied().unwrap_or("stop monitor failed");
                drop(pipe);
                let status = match timeout(SHUTDOWN_TIMEOUT, worker.wait()).await {
                    Ok(result) => result,
                    Err(_) => {
                        tracing::warn!(reason, "agent worker did not stop in time; terminating it");
                        worker.start_kill().context("cannot terminate the agent worker")?;
                        worker.wait().await
                    }
                }.context("cannot wait for the agent worker")?;
                tracing::info!(%status, code = status.code(), reason, "agent worker stopped");
                return result.map(|_| ());
            }
            status = worker.wait() => status.context("cannot wait for the agent worker")?,
        };
        if started.elapsed() >= STABLE_RUN {
            backoff = INITIAL_BACKOFF;
        }
        tracing::warn!(%status, code = status.code(), delay_seconds = backoff.as_secs(), "agent worker exited without a shutdown request; restarting after backoff");
        tokio::select! {
            biased;
            result = &mut stop => {
                let reason = result.as_ref().copied().unwrap_or("stop monitor failed");
                tracing::info!(%status, code = status.code(), reason, "agent worker restart canceled");
                return result.map(|_| ());
            }
            _ = sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::Path};

    const WORKER_LOG: &str = "NANOCTL_TEST_SUPERVISOR_LOG";
    const WORKER_MODE: &str = "NANOCTL_TEST_SUPERVISOR_MODE";
    const TEST_TIMEOUT: Duration = Duration::from_secs(15);

    fn worker_command(log: &Path, mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "supervisor::tests::worker_fixture"])
            .env(WORKER_LOG, log)
            .env(WORKER_MODE, mode)
            .stdout(Stdio::null());
        command
    }

    #[test]
    fn worker_fixture() {
        let Some(log) = std::env::var_os(WORKER_LOG) else {
            return;
        };
        let previous = fs::read_to_string(&log).unwrap_or_default();
        let started = format!("{previous}started\n");
        fs::write(&log, &started).unwrap();
        match std::env::var(WORKER_MODE).unwrap().as_str() {
            "fail-once" if previous.is_empty() => std::process::exit(7),
            "exit-zero-once" if previous.is_empty() => {}
            _ => {
                parent_shutdown().unwrap().blocking_recv().unwrap();
                fs::write(&log, format!("{started}stopped\n")).unwrap();
            }
        }
    }

    async fn stop_after_worker_starts(log: &Path, mode: &str, marker: &str) {
        timeout(
            TEST_TIMEOUT,
            supervise(worker_command(log, mode), async {
                while fs::read_to_string(log).as_deref().ok() != Some(marker) {
                    sleep(Duration::from_millis(10)).await;
                }
                Ok("stop request")
            }),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            fs::read_to_string(log).unwrap(),
            format!("{marker}stopped\n")
        );
    }

    #[tokio::test]
    async fn abnormal_worker_exit_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("worker.log");
        let started = Instant::now();
        stop_after_worker_starts(&log, "fail-once", "started\nstarted\n").await;
        assert!(started.elapsed() >= INITIAL_BACKOFF);
    }

    #[tokio::test]
    async fn unexpected_exit_zero_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("worker.log");
        stop_after_worker_starts(&log, "exit-zero-once", "started\nstarted\n").await;
    }

    #[tokio::test]
    async fn requested_shutdown_waits_for_worker_to_exit_without_restarting() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("worker.log");
        stop_after_worker_starts(&log, "wait-for-parent", "started\n").await;
    }
}
