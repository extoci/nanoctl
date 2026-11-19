use std::os::fd::AsRawFd;
use std::sync::{Arc, atomic::Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use xcap::Monitor;
use xcb::{Xid, damage, randr, x, xfixes};

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

    fn should_capture(
        &mut self,
        now: Instant,
        damaged: bool,
        initial: bool,
        fallback: bool,
    ) -> bool {
        if !damaged && now.duration_since(self.last_change) >= PARK_AFTER {
            self.parked = true;
        }
        initial || damaged || fallback
    }
}

struct DamageWatch {
    connection: xcb::Connection,
    damage: damage::Damage,
    root: x::Window,
    fast_capture: bool,
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
        let screen = connection
            .get_setup()
            .roots()
            .nth(screen as usize)
            .context("X11 screen unavailable")?;
        let fast_capture = connection.get_setup().image_byte_order() == x::ImageOrder::LsbFirst
            && connection
                .get_setup()
                .pixmap_formats()
                .iter()
                .any(|format| {
                    format.depth() == screen.root_depth() && format.bits_per_pixel() == 32
                })
            && screen
                .allowed_depths()
                .flat_map(|depth| depth.visuals())
                .any(|visual| {
                    visual.visual_id() == screen.root_visual()
                        && visual.red_mask() == 0xff0000
                        && visual.green_mask() == 0xff00
                        && visual.blue_mask() == 0xff
                });
        Ok(Self {
            connection,
            damage,
            root,
            fast_capture,
        })
    }

    fn capture(&self, monitor_id: u32) -> Result<xcap::image::RgbaImage> {
        let monitors = self
            .connection
            .wait_for_reply(self.connection.send_request(&randr::GetMonitors {
                window: self.root,
                get_active: true,
            }))?;
        let region = monitors
            .monitors()
            .find(|monitor| {
                monitor
                    .outputs()
                    .iter()
                    .any(|output| output.resource_id() == monitor_id)
            })
            .context("X11 monitor disappeared")?;
        let reply = self
            .connection
            .wait_for_reply(self.connection.send_request(&x::GetImage {
                format: x::ImageFormat::ZPixmap,
                drawable: x::Drawable::Window(self.root),
                x: region.x(),
                y: region.y(),
                width: region.width(),
                height: region.height(),
                plane_mask: u32::MAX,
            }))?;
        let width = u32::from(region.width());
        let height = u32::from(region.height());
        let mut pixels = reply.data().to_vec();
        bgra_to_rgba(&mut pixels, width, height)?;
        xcap::image::RgbaImage::from_raw(width, height, pixels).context("Invalid X11 image")
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
        let result = unsafe { libc::poll(&mut fd, 1, if parked { 100 } else { 16 }) };
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

fn bgra_to_rgba(pixels: &mut [u8], width: u32, height: u32) -> Result<()> {
    let length = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4));
    if width == 0 || height == 0 || length != Some(pixels.len()) {
        bail!("Invalid X11 BGRA image size");
    }
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
        pixel[3] = 255;
    }
    Ok(())
}

pub(super) fn start(
    monitor: &Monitor,
    stats: Arc<PipelineStats>,
) -> Result<(Arc<LatestFrame>, JoinHandle<()>)> {
    let watch = DamageWatch::new()?;
    let monitor_id = monitor.id()?;
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
        let mut initial = true;
        while let Some(latest) = weak.upgrade() {
            if latest.stopped.load(Ordering::Acquire) { break; }
            let mut damaged = false;
            if let Some(events) = watch.as_ref() {
                match events.drain() {
                    Ok(Some(received)) => {
                        damaged = true;
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
            if gate.should_capture(Instant::now(), damaged, initial, watch.is_none()) {
                let started = Instant::now();
                let image = if let Some(events) = watch.as_mut().filter(|events| events.fast_capture) {
                    match events.capture(monitor_id) {
                        Ok(image) => Ok(image),
                        Err(error) => {
                            tracing::debug!(%error, "Persistent X11 capture unavailable; retaining xcap capture");
                            events.fast_capture = false;
                            monitor.capture_image().map_err(anyhow::Error::from)
                        }
                    }
                } else { monitor.capture_image().map_err(anyhow::Error::from) };
                match image {
                    Ok(image) => {
                        initial = false;
                        stats.capture_ns.fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
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
    fn packed_x11_pixels_preserve_rgb_and_reject_invalid_layouts() {
        let mut pixels = [1, 2, 3, 0, 254, 127, 42, 99];
        bgra_to_rgba(&mut pixels, 2, 1).unwrap();
        assert_eq!(pixels, [3, 2, 1, 255, 42, 127, 254, 255]);
        assert!(bgra_to_rgba(&mut [], 0, 0).is_err());
        assert!(bgra_to_rgba(&mut pixels, 3, 1).is_err());
        assert!(bgra_to_rgba(&mut [], u32::MAX, u32::MAX).is_err());
    }

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
    fn capture_requires_damage_except_startup_and_fallback() {
        let now = Instant::now();
        let mut gate = Gate::new(now);
        assert!(gate.should_capture(now, false, true, false));
        assert!(!gate.should_capture(now, false, false, false));
        assert!(gate.should_capture(now, true, false, false));
        assert!(!gate.should_capture(now + PARK_AFTER, false, false, false));
        assert!(gate.parked);
        assert!(gate.should_capture(now + PARK_AFTER, false, false, true));
        gate.damage(now + PARK_AFTER);
        assert!(!gate.parked);
        assert!(gate.should_capture(now + PARK_AFTER, true, false, false));
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
