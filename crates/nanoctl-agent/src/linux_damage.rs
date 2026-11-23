use std::os::fd::AsRawFd;
use std::sync::{Arc, atomic::Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use xcap::Monitor;
use xcb::{Xid, damage, x, xfixes};

use super::{LatestFrame, PipelineStats};

const PARK_AFTER: Duration = Duration::from_millis(500);

struct Gate {
    last_change: Instant,
    parked: bool,
    resumed: Option<Instant>,
}

impl Gate {
    fn new(now: Instant) -> Self {
        Self {
            last_change: now,
            parked: false,
            resumed: None,
        }
    }

    fn damage(&mut self, now: Instant) {
        if self.parked {
            self.parked = false;
            self.resumed = Some(now);
        }
    }

    fn frame(&mut self, now: Instant, changed: bool) -> Option<Instant> {
        let resumed = self.resumed.take();
        if changed {
            self.last_change = now;
            resumed
        } else {
            self.parked = resumed.is_some() || now.duration_since(self.last_change) >= PARK_AFTER;
            None
        }
    }
}

struct DamageWatch {
    connection: xcb::Connection,
    damage: damage::Damage,
}

impl DamageWatch {
    fn new() -> Result<Self> {
        if std::env::var("XDG_SESSION_TYPE").is_ok_and(|s| s == "wayland")
            || std::env::var("WAYLAND_DISPLAY").is_ok_and(|s| s.to_lowercase().contains("wayland"))
        {
            bail!("Wayland session");
        }
        let (connection, screen) =
            xcb::Connection::connect_with_extensions(None, &[], &[xcb::Extension::Damage])?;
        if !connection
            .active_extensions()
            .any(|extension| extension == xcb::Extension::Damage)
        {
            bail!("XDamage extension unavailable");
        }
        connection.wait_for_reply(connection.send_request(&damage::QueryVersion {
            client_major_version: 1,
            client_minor_version: 1,
        }))?;
        let root = connection
            .get_setup()
            .roots()
            .nth(screen as usize)
            .context("X11 root window unavailable")?
            .root();
        let damage = connection.generate_id();
        connection.check_request(connection.send_request_checked(&damage::Create {
            damage,
            drawable: x::Drawable::Window(root),
            level: damage::ReportLevel::NonEmpty,
        }))?;
        connection.flush()?;
        Ok(Self { connection, damage })
    }

    fn drain(&self) -> Result<Option<Instant>> {
        let mut damaged = None;
        while let Some(event) = self.connection.poll_for_event()? {
            if matches!(event, xcb::Event::Damage(damage::Event::Notify(ref event)) if event.damage() == self.damage)
            {
                damaged.get_or_insert_with(Instant::now);
            }
        }
        if damaged.is_some() {
            self.connection
                .check_request(self.connection.send_request_checked(&damage::Subtract {
                    damage: self.damage,
                    repair: xfixes::Region::none(),
                    parts: xfixes::Region::none(),
                }))?;
        }
        Ok(damaged)
    }

    fn wait(&self, parked: bool) -> Result<()> {
        let mut fd = libc::pollfd {
            fd: self.connection.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // The connection owns this descriptor for the entire poll call.
        let result = unsafe { libc::poll(&mut fd, 1, if parked { 100 } else { 1 }) };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error.into());
            }
        }
        if fd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            bail!("XDamage connection closed");
        }
        Ok(())
    }
}

pub(super) fn start(
    monitor: &Monitor,
    stats: Arc<PipelineStats>,
) -> Result<(Arc<LatestFrame>, JoinHandle<()>)> {
    let watch = DamageWatch::new()?;
    let monitor = monitor.clone();
    let latest = Arc::new(LatestFrame {
        stats: stats.clone(),
        ..Default::default()
    });
    let weak = Arc::downgrade(&latest);
    stats.damage_enabled.store(true, Ordering::Relaxed);
    stats.capture_parked.store(false, Ordering::Relaxed);
    stats.damage_started.lock().unwrap().take();
    let thread = std::thread::Builder::new().name("nanoctl-xdamage".to_owned()).spawn(move || {
        let mut watch = Some(watch);
        let mut gate = Gate::new(Instant::now());
        while let Some(latest) = weak.upgrade() {
            if latest.stopped.load(Ordering::Acquire) { break; }
            if let Some(events) = watch.as_ref() {
                match events.drain() {
                    Ok(Some(received)) => {
                        if gate.parked { stats.damage_resumes.fetch_add(1, Ordering::Relaxed); }
                        gate.damage(received);
                    }
                    Ok(None) => {}
                    Err(error) => {
                        tracing::debug!(%error, "XDamage failed; retaining continuous capture");
                        watch = None;
                        gate.parked = false;
                        stats.damage_enabled.store(false, Ordering::Relaxed);
                    }
                }
            }
            if !gate.parked {
                match monitor.capture_image() {
                    Ok(image) => {
                        let changed = latest.last.lock().unwrap().as_ref().is_none_or(|old| {
                            old.frame.width != image.width() || old.frame.height != image.height()
                                || old.frame.raw.as_slice() != image.as_raw().as_slice()
                        });
                        let resumed = gate.frame(Instant::now(), changed);
                        if watch.is_none() { gate.parked = false; }
                        if let Some(started) = resumed {
                            stats.damage_started.lock().unwrap().get_or_insert(started);
                        }
                        if !latest.replace(xcap::Frame::new(image.width(), image.height(), image.into_raw())) { break; }
                    }
                    Err(error) => {
                        tracing::debug!(%error, "X11 capture failed");
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
            }
            stats.capture_parked.store(gate.parked, Ordering::Relaxed);
            if let Some(events) = watch.as_ref() {
                if let Err(error) = events.wait(gate.parked) {
                    tracing::debug!(%error, "XDamage wait failed; retaining continuous capture");
                    watch = None;
                    gate.parked = false;
                    stats.damage_enabled.store(false, Ordering::Relaxed);
                }
            } else {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        if let Some(latest) = weak.upgrade() {
            latest.finished.store(true, Ordering::Release);
            latest.ready.notify_one();
        }
    })?;
    Ok((latest, thread))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn park_resume_and_empty_repaints() {
        let now = Instant::now();
        let mut gate = Gate::new(now);
        assert_eq!(gate.frame(now + PARK_AFTER / 2, false), None);
        assert!(!gate.parked);
        gate.frame(now + PARK_AFTER, false);
        assert!(gate.parked);
        let event = now + Duration::from_secs(10);
        gate.damage(event);
        assert!(!gate.parked);
        assert_eq!(
            gate.frame(event + Duration::from_millis(5), true),
            Some(event)
        );
        assert_eq!(gate.frame(event + Duration::from_millis(6), true), None);
        gate.frame(event + Duration::from_secs(1), false);
        assert!(gate.parked);
        gate.damage(event + Duration::from_secs(2));
        assert_eq!(gate.frame(event + Duration::from_secs(2), false), None);
        assert!(gate.parked);
    }

    #[test]
    fn continuous_changes_prevent_parking_and_active_damage_is_not_a_resume() {
        let now = Instant::now();
        let mut gate = Gate::new(now);
        for seconds in 1..10 {
            let time = now + Duration::from_secs(seconds);
            gate.damage(time);
            assert_eq!(gate.frame(time, true), None);
            assert!(!gate.parked);
        }
    }
}
