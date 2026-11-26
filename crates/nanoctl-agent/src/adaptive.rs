use std::collections::VecDeque;
use std::time::{Duration, Instant};

const RTT_WINDOW: Duration = Duration::from_secs(30);
const RTT_FILTER_WINDOW: Duration = Duration::from_millis(1_500);
const RTT_JITTER_WINDOW: Duration = Duration::from_secs(3);
const MIN_BITRATE_KBPS: u32 = 250;
const REFRESH_DELAY: Duration = Duration::from_millis(250);
const SLOW_PROBE_GAIN: f64 = 1.08;
pub const IDLE_KEEPALIVE: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug)]
pub struct LinkSample {
    pub fraction_lost: f64,
    pub rtt: Option<Duration>,
    pub remb_kbps: Option<u32>,
    pub at: Instant,
    pub receiver_report_at: Option<Instant>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateDecision {
    pub bitrate_kbps: u32,
    pub fps: u16,
}

#[derive(Clone, Copy, Default)]
struct DelaySignal {
    rtt: Option<Duration>,
    baseline: Option<Duration>,
    trend: Option<f64>,
    rising: bool,
    congested: bool,
    draining: bool,
}

/// Caps a viewer asked for over the reliable control channel. They can lower the configured
/// `quality.max_fps` / `quality.max_bitrate_kbps` for one session, never exceed them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewerLimits {
    pub max_fps: Option<u16>,
    pub max_bitrate_kbps: Option<u32>,
    /// Physical pixels available to the rendered video. They can lower the
    /// configured encode dimensions for one session, never exceed them.
    pub max_width: Option<u32>,
    pub max_height: Option<u32>,
}

impl ViewerLimits {
    #[cfg_attr(not(feature = "media"), allow(dead_code))]
    pub fn parse(data: &[u8]) -> Option<Self> {
        #[derive(serde::Deserialize)]
        struct Envelope {
            #[serde(rename = "type")]
            kind: String,
            #[serde(flatten)]
            limits: ViewerLimits,
        }
        if !data.windows(9).any(|window| window == b"\"quality\"") {
            return None;
        }
        let envelope: Envelope = serde_json::from_slice(data).ok()?;
        (envelope.kind == "quality").then_some(envelope.limits)
    }

    #[cfg_attr(not(feature = "media"), allow(dead_code))]
    pub fn resolve_dimensions(self, max_width: u32, max_height: u32) -> (u32, u32) {
        (
            self.max_width
                .filter(|cap| *cap >= 2)
                .map_or(max_width, |cap| cap.min(max_width) & !1),
            self.max_height
                .filter(|cap| *cap >= 2)
                .map_or(max_height, |cap| cap.min(max_height) & !1),
        )
    }

    #[cfg_attr(not(feature = "media"), allow(dead_code))]
    pub fn resolve(self, max_bitrate_kbps: u32, max_fps: u16) -> (u32, u16) {
        (
            self.max_bitrate_kbps.map_or(max_bitrate_kbps, |cap| {
                cap.clamp(MIN_BITRATE_KBPS.min(max_bitrate_kbps), max_bitrate_kbps)
            }),
            self.max_fps
                .map_or(max_fps, |cap| cap.clamp(15.min(max_fps), max_fps)),
        )
    }
}

/// Viewer-measured data-channel link quality, sent as JSON on the reliable
/// control channel. RTP feedback is blind while video rides the data channel,
/// so this is the signal that lets the rate controller adapt to congestion.
#[cfg_attr(not(feature = "media"), allow(dead_code))]
pub fn parse_link_stats(data: &[u8]) -> Option<f64> {
    #[derive(serde::Deserialize)]
    struct Envelope {
        #[serde(rename = "type")]
        kind: String,
        loss: Option<f64>,
    }
    if !data.windows(12).any(|window| window == b"\"link-stats\"") {
        return None;
    }
    let envelope: Envelope = serde_json::from_slice(data).ok()?;
    (envelope.kind == "link-stats").then_some(envelope.loss?)
}

pub struct RateController {
    ceiling: u32,
    max_fps: u16,
    rate: f64,
    fps: u16,
    reported: RateDecision,
    last_sample: Option<Instant>,
    rtts: VecDeque<(Instant, Duration)>,
    last_delay: Option<(Instant, DelaySignal)>,
    last_delay_cut: Option<Instant>,
    last_congestion: Option<Instant>,
    congestion_rate: Option<f64>,
    clean_since: Option<Instant>,
    low_fps_since: Option<Instant>,
    send_kbps: Option<f64>,
    app_limited: bool,
    active_since: Option<Instant>,
    last_remb: Option<u32>,
    last_loss_report: Option<Instant>,
    high_loss_reports: u8,
}

impl RateController {
    pub fn new(ceiling_kbps: u32, max_fps: u16) -> Self {
        // Start high enough for a presentable first frame — a 4Mbps opening undershoots a
        // LAN-scale link badly, and the probe-up path recovers in seconds on a real WAN.
        let initial = (ceiling_kbps / 4)
            .min(6_000)
            .clamp(ceiling_kbps.min(MIN_BITRATE_KBPS), ceiling_kbps);
        let max_fps = max_fps.max(1);
        let fps = Self::tier(initial as f64, max_fps);
        Self {
            ceiling: ceiling_kbps,
            max_fps,
            rate: initial as f64,
            fps,
            reported: RateDecision {
                bitrate_kbps: initial,
                fps,
            },
            last_sample: None,
            rtts: VecDeque::new(),
            last_delay: None,
            last_delay_cut: None,
            last_congestion: None,
            congestion_rate: None,
            clean_since: None,
            low_fps_since: None,
            send_kbps: None,
            app_limited: false,
            active_since: None,
            last_remb: None,
            last_loss_report: None,
            high_loss_reports: 0,
        }
    }

    /// Replaces the bitrate ceiling and frame-rate cap mid-session. The current estimate is kept
    /// below the new ceiling and the frame-rate tier is recomputed immediately.
    #[cfg_attr(not(feature = "media"), allow(dead_code))]
    pub fn set_limits(&mut self, ceiling_kbps: u32, max_fps: u16) -> RateDecision {
        self.ceiling = ceiling_kbps;
        self.max_fps = max_fps.max(1);
        self.rate = self.rate.clamp(
            ceiling_kbps.min(MIN_BITRATE_KBPS) as f64,
            ceiling_kbps as f64,
        );
        self.fps = Self::tier(self.rate, self.max_fps);
        self.low_fps_since = None;
        self.reported = self.decision();
        self.reported
    }

    pub fn decision(&self) -> RateDecision {
        RateDecision {
            bitrate_kbps: self.rate.floor() as u32,
            fps: self.fps,
        }
    }

    pub fn update_sender(&mut self, kbps: f64, app_limited: bool, at: Instant) {
        self.send_kbps = (kbps.is_finite() && kbps >= 0.0).then_some(kbps);
        if app_limited {
            self.active_since = None;
            self.high_loss_reports = 0;
        } else {
            self.active_since.get_or_insert(at);
        }
        self.app_limited = app_limited;
    }

    fn tier(rate: f64, max_fps: u16) -> u16 {
        // Bitrate is the congestion knob, framerate mostly isn't: a 1080p60 stream
        // dropping to 15-20fps reads as broken to a viewer, while the same budget at
        // 60fps just looks softer. Hold 60fps (or the configured ceiling) down to
        // 500kbps, and never go below 30.
        if rate >= 2_500.0 {
            max_fps
        } else if rate >= 500.0 {
            max_fps.min(60)
        } else {
            max_fps.min(30)
        }
    }

    fn delay_signal(&mut self, sample: LinkSample, fresh_report: bool) -> DelaySignal {
        let previous = self
            .last_delay
            .filter(|(at, _)| sample.at.duration_since(*at) <= RTT_FILTER_WINDOW);
        if !fresh_report {
            return previous.map_or_else(DelaySignal::default, |(_, signal)| signal);
        }
        let Some(rtt) = sample.rtt else {
            self.last_delay = None;
            return DelaySignal::default();
        };
        while self
            .rtts
            .front()
            .is_some_and(|(at, _)| sample.at.duration_since(*at) > RTT_WINDOW)
        {
            self.rtts.pop_front();
        }
        let raw_trend = self.rtts.back().and_then(|(at, previous)| {
            let seconds = sample.at.duration_since(*at).as_secs_f64();
            (0.1..=RTT_FILTER_WINDOW.as_secs_f64())
                .contains(&seconds)
                .then(|| (rtt.as_secs_f64() - previous.as_secs_f64()) / seconds)
        });
        self.rtts.push_back((sample.at, rtt));
        let filtered = self
            .rtts
            .iter()
            .rev()
            .take_while(|(at, _)| sample.at.duration_since(*at) <= RTT_FILTER_WINDOW)
            .map(|(_, rtt)| *rtt)
            .min()
            .unwrap_or(rtt);
        let baseline = self.rtts.iter().map(|(_, rtt)| *rtt).min().unwrap_or(rtt);
        let trend = previous.and_then(|(at, signal)| {
            let seconds = sample.at.duration_since(at).as_secs_f64();
            let previous_rtt = signal.rtt?;
            (seconds >= 0.1)
                .then(|| (filtered.as_secs_f64() - previous_rtt.as_secs_f64()) / seconds)
        });
        let rising = filtered.saturating_sub(baseline) >= Duration::from_millis(15)
            && trend.is_some_and(|slope| slope > 0.015)
            && raw_trend.is_some_and(|slope| slope > 0.015)
            && previous.is_some_and(|(_, signal)| signal.trend.is_some_and(|slope| slope > 0.015));
        let queue_limit = (baseline / 4).max(Duration::from_millis(30));
        let sending_at_target = self.reported.bitrate_kbps >= MIN_BITRATE_KBPS * 2
            && self
                .send_kbps
                .is_none_or(|sent| sent >= f64::from(self.reported.bitrate_kbps) * 0.85);
        let congested = if sending_at_target {
            rtt.saturating_sub(baseline) > queue_limit
        } else {
            let (min, max) = self
                .rtts
                .iter()
                .rev()
                .take_while(|(at, _)| sample.at.duration_since(*at) <= RTT_JITTER_WINDOW)
                .fold((rtt, rtt), |(min, max), (_, rtt)| {
                    (min.min(*rtt), max.max(*rtt))
                });
            filtered.saturating_sub(baseline) > queue_limit.max(max - min)
        };
        let signal = DelaySignal {
            rtt: Some(filtered),
            baseline: Some(baseline),
            trend,
            rising,
            congested: congested || rising,
            draining: (if sending_at_target { raw_trend } else { trend })
                .is_some_and(|slope| slope < -0.005),
        };
        self.last_delay = Some((sample.at, signal));
        signal
    }

    pub fn observe(&mut self, sample: LinkSample) -> Option<RateDecision> {
        if self.last_sample.is_some_and(|last| sample.at < last)
            || !sample.fraction_lost.is_finite()
        {
            return None;
        }
        let elapsed = self
            .last_sample
            .map_or(0.0, |last| sample.at.duration_since(last).as_secs_f64());
        self.last_sample = Some(sample.at);
        let new_remb = sample
            .remb_kbps
            .filter(|remb| Some(*remb) != self.last_remb);
        self.last_remb = sample.remb_kbps.or(self.last_remb);
        let loss = sample.fraction_lost.clamp(0.0, 1.0);
        let previous_report = self.last_loss_report;
        let fresh_report = sample
            .receiver_report_at
            .is_some_and(|at| previous_report.is_none_or(|last| at > last));
        if fresh_report {
            self.last_loss_report = sample.receiver_report_at;
            self.high_loss_reports = if loss > 0.1 && !self.app_limited {
                self.high_loss_reports.saturating_add(1)
            } else {
                0
            };
        }
        let sustained_loss = self.high_loss_reports >= 2;
        let loss_cut = loss > 0.1 && sustained_loss && fresh_report;
        let previous_rate = self.rate;
        let delay = self.delay_signal(sample, fresh_report);
        let delay_congested = delay.congested;
        let mut remb_cut = false;
        let delay_cut = fresh_report
            && delay_congested
            && !self.app_limited
            && !delay.draining
            && delay.rtt.is_some_and(|rtt| {
                self.last_delay_cut.is_none_or(|last| {
                    sample.at.duration_since(last)
                        >= rtt.clamp(Duration::from_millis(300), Duration::from_secs(1))
                })
            });
        if delay_cut {
            self.last_delay_cut = Some(sample.at);
        }
        if loss < 0.02 && !delay_congested && delay.rtt.is_some() && !self.app_limited {
            self.clean_since.get_or_insert(sample.at);
        } else {
            self.clean_since = None;
        }
        if self.app_limited {
            return None;
        }
        let clean_for = self
            .clean_since
            .map_or(Duration::ZERO, |since| sample.at.duration_since(since));
        if clean_for >= Duration::from_secs(10) {
            self.last_congestion = None;
            self.congestion_rate = None;
        }
        if loss_cut || delay_cut {
            self.last_congestion = Some(sample.at);
            self.congestion_rate = Some(self.rate);
        }
        if loss_cut {
            self.rate *= 1.0 - 0.5 * loss;
        } else if loss < 0.02 && !delay_congested {
            if let Some(last) = self.last_congestion {
                if clean_for >= Duration::from_secs(3) {
                    let gain = if delay
                        .rtt
                        .is_some_and(|rtt| rtt <= Duration::from_millis(10))
                    {
                        2.0_f64
                    } else {
                        Self::probe_gain(delay.rtt)
                    };
                    self.rate *= gain.powf(elapsed.min(2.0));
                } else if sample.at.duration_since(last) >= Duration::from_secs(3) {
                    let near_congestion = self
                        .congestion_rate
                        .is_some_and(|rate| self.rate >= rate * 0.85);
                    let step = if near_congestion {
                        (self.rate * 0.005).min(10.0)
                    } else {
                        (self.rate * 0.02).min(30.0)
                    };
                    self.rate += step * elapsed.min(2.0);
                }
            } else {
                let slow_elapsed = if sample.rtt.is_some() {
                    elapsed.min(2.0)
                } else {
                    elapsed
                };
                self.rate *= SLOW_PROBE_GAIN.powf(slow_elapsed);
                if fresh_report
                    && clean_for >= Duration::from_secs(3)
                    && let Some((previous, current)) =
                        previous_report.zip(sample.receiver_report_at)
                {
                    let gain = Self::probe_gain(delay.rtt);
                    let clean_gain = SLOW_PROBE_GAIN + (gain - SLOW_PROBE_GAIN) * 1.5;
                    let report_elapsed = current.duration_since(previous).as_secs_f64().min(2.0);
                    self.rate *= (clean_gain / SLOW_PROBE_GAIN).powf(report_elapsed);
                }
            }
        }
        if delay_cut {
            self.rate *= 0.85;
        }
        let floor_probe = self.rate <= f64::from(MIN_BITRATE_KBPS * 2) && loss == 0.0;
        let remb_corroborated =
            sustained_loss || delay_congested || (sample.rtt.is_none() && !floor_probe);
        if let Some(remb) = new_remb
            && remb_corroborated
            && self
                .send_kbps
                .is_some_and(|sent| f64::from(remb) < sent * 0.95)
            && self.active_since.is_none_or(|since| {
                sample.at.saturating_duration_since(since) >= Duration::from_secs(2)
            })
        {
            let cap = f64::from(remb) * 0.95;
            if cap < self.rate {
                remb_cut = true;
                self.last_congestion = Some(sample.at);
                self.congestion_rate = Some(self.rate);
                self.clean_since = None;
                self.rate = cap;
            }
        }
        self.rate = self.rate.clamp(
            self.ceiling.min(MIN_BITRATE_KBPS) as f64,
            self.ceiling as f64,
        );

        let target_fps = Self::tier(self.rate, self.max_fps);
        if target_fps < self.fps {
            let since = *self.low_fps_since.get_or_insert(sample.at);
            if sample.at.duration_since(since) >= Duration::from_secs(1) {
                self.fps = target_fps;
                self.low_fps_since = None;
            }
        } else {
            self.low_fps_since = None;
            if target_fps > self.fps {
                self.fps = Self::tier(self.rate / 1.2, self.max_fps).max(self.fps);
            }
        }
        let decision = self.decision();
        let delta = decision.bitrate_kbps.abs_diff(self.reported.bitrate_kbps) as f64;
        let changed = decision.fps != self.reported.fps
            || (decision.bitrate_kbps == self.ceiling
                && self.reported.bitrate_kbps != self.ceiling)
            || delta >= (self.reported.bitrate_kbps as f64 * 0.1).max(1.0);
        if changed || loss_cut || delay_cut || remb_cut {
            let reason = if remb_cut {
                "remb"
            } else if loss_cut {
                "loss"
            } else if delay_cut && delay.rising {
                "trend"
            } else if delay_cut {
                "rtt"
            } else if decision.fps != self.reported.fps && self.rate == previous_rate {
                "fps"
            } else {
                "probe"
            };
            tracing::info!(
                reason,
                previous_kbps = previous_rate,
                target_kbps = decision.bitrate_kbps,
                target_fps = decision.fps,
                applied = changed,
                loss,
                loss_cut,
                high_loss_reports = self.high_loss_reports,
                delay_cut,
                remb_cut,
                remb_corroborated,
                rtt_ms = sample.rtt.map(|rtt| rtt.as_secs_f64() * 1000.0),
                filtered_rtt_ms = delay.rtt.map(|rtt| rtt.as_secs_f64() * 1000.0),
                baseline_ms = delay.baseline.map(|rtt| rtt.as_secs_f64() * 1000.0),
                trend_ms_per_s = delay.trend.map(|slope| slope * 1000.0),
                remb_kbps = sample.remb_kbps,
                send_kbps = self.send_kbps,
                "video rate decision"
            );
        }
        if changed {
            self.reported = decision;
            Some(decision)
        } else {
            None
        }
    }

    fn probe_gain(rtt: Option<Duration>) -> f64 {
        rtt.map_or(SLOW_PROBE_GAIN, |rtt| {
            (1.0 + 0.004 / rtt.as_secs_f64().max(0.001)).clamp(SLOW_PROBE_GAIN, 1.5)
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaceAction {
    Encode,
    Skip,
}

pub struct DamagePacer {
    fps: u16,
    last_encode: Option<Instant>,
    last_change: Option<Instant>,
    pending: bool,
    refresh_pending: bool,
}

impl DamagePacer {
    pub fn new(fps: u16) -> Self {
        Self {
            fps: fps.max(1),
            last_encode: None,
            last_change: None,
            pending: false,
            refresh_pending: false,
        }
    }

    pub fn set_fps(&mut self, fps: u16) {
        self.fps = fps.max(1);
    }

    pub fn idle(&self) -> bool {
        !self.pending && !self.refresh_pending
    }

    fn interval(fps: u16) -> Duration {
        Duration::from_secs_f64(0.5 / fps.max(1) as f64)
    }

    pub fn next_deadline(&self, now: Instant, fps: u16) -> Instant {
        let Some(last) = self.last_encode else {
            return now;
        };
        let due = if self.pending {
            last + Self::interval(fps)
        } else if self.refresh_pending {
            (self.last_change.unwrap() + REFRESH_DELAY).max(last + Self::interval(fps))
        } else {
            last + IDLE_KEEPALIVE
        };
        due.max(now)
    }

    pub fn on_frame(&mut self, now: Instant, changed: bool, forced_keyframe: bool) -> PaceAction {
        self.on_frame_inner(now, changed, forced_keyframe, Duration::ZERO)
    }

    #[cfg(any(target_os = "windows", test))]
    pub fn on_capture_frame(
        &mut self,
        now: Instant,
        changed: bool,
        forced_keyframe: bool,
    ) -> PaceAction {
        if !changed {
            return self.on_frame(now, changed, forced_keyframe);
        }
        let tolerance = Duration::from_millis(2).min(Self::interval(self.fps) / 10);
        self.on_frame_inner(now, changed, forced_keyframe, tolerance)
    }

    fn on_frame_inner(
        &mut self,
        now: Instant,
        changed: bool,
        forced_keyframe: bool,
        tolerance: Duration,
    ) -> PaceAction {
        if changed {
            self.pending = true;
            self.refresh_pending = true;
            self.last_change = Some(now);
        }
        let ready = self.last_encode.is_none_or(|last| {
            now.saturating_duration_since(last)
                >= Self::interval(self.fps).saturating_sub(tolerance)
        });
        let refresh = self.refresh_pending
            && self
                .last_change
                .is_some_and(|last| now.saturating_duration_since(last) >= REFRESH_DELAY);
        let keepalive = self
            .last_encode
            .is_some_and(|last| now.saturating_duration_since(last) >= IDLE_KEEPALIVE);
        if forced_keyframe || (ready && (self.pending || refresh || keepalive)) {
            self.last_encode = Some(now);
            self.pending = false;
            if refresh {
                self.refresh_pending = false;
            }
            PaceAction::Encode
        } else {
            PaceAction::Skip
        }
    }
}

pub fn frame_changed(prev: &[u8], next: &[u8]) -> bool {
    prev != next
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewer_limits_lower_but_never_exceed_the_configured_maximums() {
        let limits =
            ViewerLimits::parse(br#"{"type":"quality","maxFps":30,"maxBitrateKbps":10000}"#)
                .unwrap();
        assert_eq!(limits.resolve(50_000, 60), (10_000, 30));
        let raised =
            ViewerLimits::parse(br#"{"type":"quality","maxFps":240,"maxBitrateKbps":900000}"#)
                .unwrap();
        assert_eq!(raised.resolve(50_000, 120), (50_000, 120));
        assert_eq!(ViewerLimits::default().resolve(50_000, 120), (50_000, 120));
        assert_eq!(
            ViewerLimits::parse(br#"{"type":"quality","maxFps":1}"#)
                .unwrap()
                .resolve(50_000, 60),
            (50_000, 15)
        );
        assert!(ViewerLimits::parse(br#"{"type":"key","code":"quality"}"#).is_none());
        assert!(ViewerLimits::parse(br#"{"type":"display","displayId":"1"}"#).is_none());
    }

    #[test]
    fn viewer_dimension_limits_are_even_and_never_exceed_configured_maximums() {
        let limits =
            ViewerLimits::parse(br#"{"type":"quality","maxWidth":1281,"maxHeight":719}"#).unwrap();
        assert_eq!(limits.resolve_dimensions(3840, 2160), (1280, 718));
        let raised =
            ViewerLimits::parse(br#"{"type":"quality","maxWidth":7680,"maxHeight":4320}"#).unwrap();
        assert_eq!(raised.resolve_dimensions(3840, 2160), (3840, 2160));
        assert_eq!(
            ViewerLimits::default().resolve_dimensions(3840, 2160),
            (3840, 2160)
        );
    }

    #[test]
    fn set_limits_clamps_rate_and_recomputes_fps() {
        let mut controller = RateController::new(50_000, 120);
        let initial = controller.decision();
        assert_eq!(
            initial,
            RateDecision {
                bitrate_kbps: 6_000,
                fps: 120
            }
        );
        assert_eq!(
            controller.set_limits(2_000, 60),
            RateDecision {
                bitrate_kbps: 2_000,
                fps: 60
            }
        );
        assert_eq!(
            controller.set_limits(50_000, 120),
            RateDecision {
                bitrate_kbps: 2_000,
                fps: 60
            }
        );
        assert_eq!(
            controller.set_limits(50_000, 24),
            RateDecision {
                bitrate_kbps: 2_000,
                fps: 24
            }
        );
    }

    fn sample(at: Instant, loss: f64) -> LinkSample {
        LinkSample {
            fraction_lost: loss,
            rtt: None,
            remb_kbps: None,
            at,
            receiver_report_at: Some(at),
        }
    }

    #[test]
    fn clean_ramp_is_elapsed_time_scaled_and_notifications_accumulate() {
        let now = Instant::now();
        let mut controller = RateController::new(8_000, 60);
        assert_eq!(controller.decision().bitrate_kbps, 2_000);
        assert_eq!(controller.observe(sample(now, 0.0)), None);
        assert_eq!(
            controller.observe(sample(now + Duration::from_secs(1), 0.0)),
            None
        );
        let decision = controller
            .observe(sample(now + Duration::from_secs(2), 0.0))
            .unwrap();
        assert_eq!(decision.bitrate_kbps, 2_332);
        let mut single = RateController::new(8_000, 60);
        single.observe(sample(now, 0.0));
        single.observe(sample(now + Duration::from_secs(2), 0.0));
        assert_eq!(single.decision(), controller.decision());
    }

    #[test]
    fn loss_cut_and_moderate_loss_hold() {
        let now = Instant::now();
        let mut controller = RateController::new(8_000, 60);
        assert_eq!(
            controller.observe(sample(now - Duration::from_secs(1), 0.2)),
            None
        );
        assert_eq!(
            controller.observe(sample(now, 0.2)).unwrap().bitrate_kbps,
            1_800
        );
        assert_eq!(
            controller.observe(sample(now + Duration::from_secs(20), 0.05)),
            None
        );
        assert_eq!(controller.decision().bitrate_kbps, 1_800);
    }

    #[test]
    fn stable_measured_link_probes_quickly_after_congestion_clears() {
        let now = Instant::now();
        let mut clean = RateController::new(8_000, 60);
        let mut cut = RateController::new(8_000, 60);
        cut.observe(sample(now - Duration::from_secs(1), 0.2));
        cut.observe(sample(now, 0.2));
        for seconds in 0..=7 {
            let mut input = sample(now + Duration::from_secs(seconds), 0.0);
            input.rtt = Some(Duration::from_millis(1));
            clean.observe(input);
            cut.observe(input);
        }
        assert_eq!(clean.decision().bitrate_kbps, 8_000);
        assert_eq!(clean.decision().fps, 60);
        assert_eq!(cut.decision().bitrate_kbps, 8_000);
    }

    #[test]
    fn moderate_loss_or_missing_rtt_resets_fast_probe_eligibility() {
        let now = Instant::now();
        for missing_rtt in [false, true] {
            let mut controller = RateController::new(8_000, 60);
            for seconds in 0..=6 {
                let mut input = sample(now + Duration::from_secs(seconds), 0.0);
                input.rtt = Some(Duration::from_millis(1));
                if seconds % 3 == 2 {
                    if missing_rtt {
                        input.rtt = None;
                    } else {
                        input.fraction_lost = 0.05;
                    }
                }
                controller.observe(input);
            }
            assert!(controller.decision().bitrate_kbps < 3_500);
        }
    }

    #[test]
    fn startup_probe_slows_with_feedback_latency() {
        let now = Instant::now();
        let mut controller = RateController::new(8_000, 60);
        for seconds in 0..=7 {
            let mut input = sample(now + Duration::from_secs(seconds), 0.0);
            input.rtt = Some(Duration::from_millis(80));
            controller.observe(input);
        }
        assert_eq!(
            controller.decision().bitrate_kbps,
            (2_000.0 * 1.08_f64.powi(7)).floor() as u32
        );
    }

    #[test]
    fn clean_feedback_reaches_a_fifty_megabit_ceiling() {
        let now = Instant::now();
        let mut controller = RateController::new(50_000, 60);
        assert_eq!(controller.decision().bitrate_kbps, 6_000);
        let mut ceiling_at = None;
        for second in 0..=14 {
            let mut input = sample(now + Duration::from_secs(second), 0.0);
            input.rtt = Some(Duration::from_millis(20));
            if let Some(decision) = controller.observe(input)
                && decision.bitrate_kbps == 50_000
            {
                assert!(ceiling_at.replace(second).is_none());
                assert_eq!(decision.fps, 60);
            }
        }
        assert!(ceiling_at.is_some_and(|second| second <= 13));
    }

    #[test]
    fn remb_copies_neither_accelerate_nor_starve_fresh_clean_probes() {
        let now = Instant::now();
        let mut fresh_only = RateController::new(24_000, 60);
        let mut interleaved = RateController::new(24_000, 60);
        for second in 0..=4 {
            let mut input = sample(now + Duration::from_secs(second), 0.0);
            input.rtt = Some(Duration::from_millis(20));
            fresh_only.observe(input);
            interleaved.observe(input);
            assert_eq!(fresh_only.decision(), interleaved.decision());
            if second < 4 {
                let before = interleaved.decision().bitrate_kbps;
                input.at += Duration::from_millis(500);
                input.remb_kbps = Some(500 + second as u32);
                interleaved.observe(input);
                let after = interleaved.decision().bitrate_kbps;
                assert!(f64::from(after) <= (f64::from(before) + 1.0) * SLOW_PROBE_GAIN.sqrt());
            }
        }
    }

    #[test]
    fn clean_measured_probe_limits_the_growth_after_feedback_goes_quiet() {
        let now = Instant::now();
        let mut controller = RateController::new(50_000, 60);
        for second in [0, 20] {
            let mut input = sample(now + Duration::from_secs(second), 0.0);
            input.rtt = Some(Duration::from_millis(1));
            controller.observe(input);
        }
        assert_eq!(controller.decision().bitrate_kbps, 17_544);
    }

    #[test]
    fn ceiling_is_reported_once_even_when_the_final_increase_is_small() {
        let now = Instant::now();
        let mut controller = RateController::new(24_000, 60);
        controller.rate = 23_900.0;
        controller.reported = controller.decision();
        assert_eq!(controller.observe(sample(now, 0.0)), None);
        assert_eq!(
            controller.observe(sample(now + Duration::from_secs(1), 0.0)),
            Some(RateDecision {
                bitrate_kbps: 24_000,
                fps: 60,
            })
        );
        assert_eq!(
            controller.observe(sample(now + Duration::from_secs(2), 0.0)),
            None
        );
    }

    #[test]
    fn delay_cut_is_throttled_and_baseline_expires() {
        let now = Instant::now();
        let mut controller = RateController::new(8_000, 60);
        let mut input = sample(now, 0.05);
        input.rtt = Some(Duration::from_millis(40));
        controller.observe(input);
        input.at += Duration::from_millis(100);
        input.receiver_report_at = Some(input.at);
        input.rtt = Some(Duration::from_millis(120));
        assert_eq!(controller.observe(input).unwrap().bitrate_kbps, 1_700);
        input.at += Duration::from_millis(100);
        input.receiver_report_at = Some(input.at);
        assert_eq!(controller.observe(input), None);
        input.at += Duration::from_millis(200);
        input.receiver_report_at = Some(input.at);
        assert_eq!(controller.observe(input).unwrap().bitrate_kbps, 1_445);
        input.at = now + Duration::from_secs(31);
        input.receiver_report_at = Some(input.at);
        controller.observe(input);
        assert_eq!(controller.decision().bitrate_kbps, 1_445);
    }

    #[test]
    fn remb_and_configured_bounds() {
        let now = Instant::now();
        let mut controller = RateController::new(8_000, 60);
        controller.update_sender(2_000.0, false, now - Duration::from_secs(3));
        let mut input = sample(now, 0.05);
        input.remb_kbps = Some(1_001);
        assert_eq!(controller.observe(input).unwrap().bitrate_kbps, 950);
        input.remb_kbps = Some(1_000);
        assert_eq!(controller.observe(input), None);
        input.remb_kbps = Some(0);
        assert_eq!(controller.observe(input).unwrap().bitrate_kbps, 250);
        let mut clean = RateController::new(8_000, 60);
        clean.observe(sample(now, 0.0));
        assert_eq!(
            clean
                .observe(sample(now + Duration::from_secs(1_000), 0.0))
                .unwrap()
                .bitrate_kbps,
            8_000
        );
        for ceiling in [0, 100, 250] {
            let mut low = RateController::new(ceiling, 10);
            low.observe(sample(now, 1.0));
            assert_eq!(
                low.decision(),
                RateDecision {
                    bitrate_kbps: ceiling,
                    fps: 10
                }
            );
        }
    }

    #[test]
    fn fps_drop_requires_one_second_and_raise_requires_headroom() {
        let now = Instant::now();
        let mut controller = RateController::new(12_000, 60);
        controller.update_sender(4_000.0, false, now - Duration::from_secs(3));
        let mut input = sample(now, 0.05);
        input.remb_kbps = Some(400);
        controller.observe(input);
        assert_eq!(controller.decision().fps, 60);
        input.at += Duration::from_millis(999);
        controller.observe(input);
        assert_eq!(controller.decision().fps, 60);
        input.at += Duration::from_millis(1);
        assert_eq!(controller.observe(input).unwrap().fps, 30);
        input.remb_kbps = None;
        input.fraction_lost = 0.0;
        input.rtt = Some(Duration::from_millis(80));
        for second in 2..=8 {
            input.at = now + Duration::from_secs(second);
            input.receiver_report_at = Some(input.at);
            controller.observe(input);
        }
        assert!(controller.decision().bitrate_kbps > 400);
        assert_eq!(controller.decision().fps, 30);
        for second in 9..=14 {
            input.at = now + Duration::from_secs(second);
            input.receiver_report_at = Some(input.at);
            controller.observe(input);
        }
        assert_eq!(controller.decision().fps, 60);
    }

    #[test]
    fn idle_remb_and_stale_loss_preserve_pre_idle_rate_and_fps() {
        let now = Instant::now();
        let mut controller = RateController::new(8_000, 60);
        for second in 0..=10 {
            let mut input = sample(now + Duration::from_secs(second), 0.0);
            input.rtt = Some(Duration::from_millis(1));
            controller.observe(input);
        }
        let before = controller.decision();
        assert_eq!(before.bitrate_kbps, 8_000);
        for second in 11..=40 {
            let at = now + Duration::from_secs(second);
            controller.update_sender(1.3, true, at);
            let mut input = sample(at, 0.5);
            input.remb_kbps = Some(12);
            input.rtt = Some(Duration::from_millis(100));
            assert_eq!(controller.observe(input), None);
            assert_eq!(controller.decision(), before);
        }
    }

    #[test]
    fn remb_only_binds_below_actual_sending_and_ignores_unknown_or_invalid_rates() {
        let now = Instant::now();
        for sent in [100.0, f64::NAN, -1.0, f64::INFINITY] {
            let mut controller = RateController::new(8_000, 60);
            controller.update_sender(sent, false, now - Duration::from_secs(3));
            let mut input = sample(now, 0.05);
            input.remb_kbps = Some(160);
            assert_eq!(controller.observe(input), None);
            assert_eq!(controller.decision().bitrate_kbps, 2_000);
        }
        let mut controller = RateController::new(8_000, 60);
        controller.update_sender(1_800.0, false, now - Duration::from_secs(3));
        let mut input = sample(now, 0.05);
        input.remb_kbps = Some(1_000);
        assert_eq!(controller.observe(input).unwrap().bitrate_kbps, 950);
        assert_eq!(controller.last_congestion, Some(now));
    }

    #[test]
    fn resumed_motion_ignores_stale_idle_remb_for_a_measurement_window() {
        let now = Instant::now();
        let mut controller = RateController::new(8_000, 60);
        controller.update_sender(1.3, true, now);
        controller.update_sender(1_800.0, false, now + Duration::from_secs(10));
        let mut input = sample(now + Duration::from_secs(11), 0.05);
        input.remb_kbps = Some(12);
        assert_eq!(controller.observe(input), None);
        assert_eq!(controller.decision().bitrate_kbps, 2_000);
        input.at += Duration::from_secs(1);
        assert_eq!(controller.observe(input), None);
        input.remb_kbps = Some(13);
        assert_eq!(controller.observe(input).unwrap().bitrate_kbps, 250);
    }

    #[test]
    fn old_remb_carried_by_receiver_reports_cannot_recap_a_clean_probe() {
        let now = Instant::now();
        let mut controller = RateController::new(8_000, 60);
        controller.update_sender(1_000.0, false, now - Duration::from_secs(3));
        let mut input = sample(now, 0.0);
        input.rtt = Some(Duration::from_millis(1));
        input.remb_kbps = Some(1_510);
        controller.observe(input);
        for second in 1..=7 {
            input.at = now + Duration::from_secs(second);
            input.receiver_report_at = Some(input.at);
            controller.update_sender(4_000.0, false, input.at);
            controller.observe(input);
        }
        assert_eq!(controller.decision().bitrate_kbps, 8_000);
        input.rtt = Some(Duration::from_millis(50));
        for second in 8..=10 {
            input.at = now + Duration::from_secs(second);
            input.receiver_report_at = Some(input.at);
            controller.observe(input);
            assert_eq!(controller.decision().bitrate_kbps, 8_000);
        }
        input.remb_kbps = Some(1_500);
        input.at += Duration::from_secs(1);
        input.receiver_report_at = Some(input.at);
        assert_eq!(controller.observe(input).unwrap().bitrate_kbps, 1_425);
    }

    #[test]
    fn clean_feedback_recovers_from_floor_with_chrome_remb_and_expires_congestion_memory() {
        let now = Instant::now();
        let mut controller = RateController::new(8_000, 60);
        controller.update_sender(2_000.0, false, now - Duration::from_secs(3));
        let mut input = sample(now, 0.0);
        input.remb_kbps = Some(100);
        controller.observe(input);
        assert_eq!(controller.decision().bitrate_kbps, 250);
        for second in 1..=11 {
            input.at = now + Duration::from_secs(second);
            input.receiver_report_at = Some(input.at);
            input.rtt = Some(Duration::from_millis(1));
            let sent = f64::from(controller.decision().bitrate_kbps) * 0.7;
            controller.update_sender(sent, false, input.at);
            input.remb_kbps = Some((1.5 * sent + 10.0) as u32);
            controller.observe(input);
            if second == 10 {
                assert!(controller.decision().bitrate_kbps >= 4_000);
                assert_eq!(controller.decision().fps, 60);
                assert!(controller.congestion_rate.is_some());
            }
        }
        assert!(controller.last_congestion.is_none());
        assert!(controller.congestion_rate.is_none());
    }

    #[test]
    fn clean_loopback_rejects_chrome_remb_collapse_despite_sending_above_estimate() {
        let now = Instant::now();
        let mut controller = RateController::new(8_000, 60);
        controller.update_sender(4_000.0, false, now - Duration::from_secs(3));
        for second in 0..=20 {
            let mut input = sample(now + Duration::from_secs(second), 0.0);
            input.rtt = Some(Duration::from_micros(500 + second % 5 * 200));
            input.remb_kbps = Some(740_u32.saturating_sub(second as u32 * 40).max(21));
            controller.observe(input);
            assert!(controller.decision().bitrate_kbps >= 2_000);
        }
        assert_eq!(controller.decision().bitrate_kbps, 8_000);
        assert_eq!(controller.decision().fps, 60);
    }

    #[test]
    fn loss_requires_two_fresh_receiver_reports_not_remb_copies() {
        let now = Instant::now();
        let mut controller = RateController::new(8_000, 60);
        let mut input = sample(now, 0.2);
        assert_eq!(controller.observe(input), None);
        input.at += Duration::from_millis(500);
        input.remb_kbps = Some(3_000);
        assert_eq!(controller.observe(input), None);
        assert_eq!(controller.high_loss_reports, 1);
        assert_eq!(
            controller
                .observe(sample(now + Duration::from_secs(1), 0.2))
                .unwrap()
                .bitrate_kbps,
            1_800
        );
        controller.observe(sample(now + Duration::from_secs(2), 0.0));
        controller.observe(sample(now + Duration::from_secs(3), 0.2));
        assert_eq!(controller.decision().bitrate_kbps, 1_800);
        let mut no_report = sample(now + Duration::from_secs(4), 0.2);
        no_report.receiver_report_at = None;
        controller.observe(no_report);
        assert_eq!(controller.decision().bitrate_kbps, 1_800);
    }

    #[test]
    fn low_rtt_trend_noise_has_an_absolute_queue_floor() {
        let now = Instant::now();
        let mut controller = RateController::new(8_000, 60);
        for tick in 0..30 {
            let mut input = sample(now + Duration::from_millis(tick * 100), 0.0);
            input.rtt = Some(Duration::from_millis(if tick % 2 == 0 { 1 } else { 3 }));
            controller.observe(input);
        }
        assert!(controller.decision().bitrate_kbps >= 2_000);
        assert!(controller.last_delay_cut.is_none());
    }

    #[test]
    fn idle_breaks_loss_report_streak() {
        let now = Instant::now();
        let mut controller = RateController::new(8_000, 60);
        controller.observe(sample(now, 0.2));
        controller.update_sender(1.0, true, now + Duration::from_secs(1));
        controller.update_sender(2_000.0, false, now + Duration::from_secs(10));
        controller.observe(sample(now + Duration::from_secs(10), 0.2));
        assert_eq!(controller.decision().bitrate_kbps, 2_000);
        assert_eq!(controller.high_loss_reports, 1);
    }

    #[test]
    fn genuine_queue_growth_corroborates_remb() {
        let now = Instant::now();
        let mut controller = RateController::new(8_000, 60);
        controller.update_sender(1_800.0, false, now - Duration::from_secs(3));
        let mut input = sample(now, 0.0);
        input.rtt = Some(Duration::from_millis(1));
        controller.observe(input);
        input.at += Duration::from_secs(1);
        input.receiver_report_at = Some(input.at);
        input.rtt = Some(Duration::from_millis(50));
        input.remb_kbps = Some(1_000);
        assert_eq!(controller.observe(input).unwrap().bitrate_kbps, 950);
    }

    #[test]
    fn invalid_and_out_of_order_samples_do_not_mutate_state() {
        let now = Instant::now();
        let mut controller = RateController::new(8_000, 60);
        controller.observe(sample(now, 0.05));
        assert_eq!(
            controller.observe(sample(now - Duration::from_secs(1), 1.0)),
            None
        );
        assert_eq!(controller.observe(sample(now, f64::NAN)), None);
        assert_eq!(controller.decision().bitrate_kbps, 2_000);
    }

    #[test]
    fn delay_congestion_prevents_clean_loss_ramp_even_between_cuts() {
        let now = Instant::now();
        let mut controller = RateController::new(8_000, 60);
        let mut input = sample(now, 0.0);
        input.rtt = Some(Duration::from_millis(40));
        controller.observe(input);
        input.at += Duration::from_secs(1);
        input.receiver_report_at = Some(input.at);
        input.rtt = Some(Duration::from_millis(120));
        assert_eq!(controller.observe(input).unwrap().bitrate_kbps, 1_700);
        input.at += Duration::from_millis(100);
        input.receiver_report_at = Some(input.at);
        assert_eq!(controller.observe(input), None);
        assert_eq!(controller.decision().bitrate_kbps, 1_700);
    }

    #[test]
    fn delay_trend_cuts_before_absolute_threshold_and_drain_does_not_recursively_cut() {
        let now = Instant::now();
        let mut controller = RateController::new(8_000, 60);
        for (second, rtt) in [160, 180, 197, 218, 300, 160].into_iter().enumerate() {
            let at = now + Duration::from_secs(second as u64);
            controller.update_sender(400.0, false, at);
            let mut input = sample(at, 0.0);
            input.rtt = Some(Duration::from_millis(rtt));
            controller.observe(input);
            let (_, delay) = controller.last_delay.unwrap();
            assert_eq!(delay.rising, (3..=4).contains(&second));
            assert_eq!(
                controller.last_delay_cut,
                match second {
                    0..=2 => None,
                    3..=4 => Some(at),
                    _ => Some(at - Duration::from_secs(1)),
                }
            );
        }
    }

    #[test]
    fn recovery_holds_then_approaches_congestion_rate_cautiously_without_rtt() {
        let now = Instant::now();
        let mut controller = RateController::new(8_000, 60);
        controller.observe(sample(now - Duration::from_secs(1), 0.2));
        controller.observe(sample(now, 0.2));
        for second in 1..=2 {
            controller.observe(sample(now + Duration::from_secs(second), 0.0));
            assert_eq!(controller.decision().bitrate_kbps, 1_800);
        }
        for second in 3..=20 {
            controller.observe(sample(now + Duration::from_secs(second), 0.0));
        }
        let recovered = controller.decision().bitrate_kbps;
        assert!((1_800..2_000).contains(&recovered));
        controller.observe(sample(now + Duration::from_secs(200), 0.0));
        assert!(controller.decision().bitrate_kbps <= recovered + 20);
    }

    #[test]
    fn small_rtt_noise_does_not_trigger_delay_backoff() {
        let now = Instant::now();
        let mut controller = RateController::new(8_000, 60);
        for second in 0..5 {
            let mut input = sample(now + Duration::from_secs(second), 0.0);
            input.rtt = Some(Duration::from_millis(80 + second % 3));
            controller.observe(input);
        }
        assert!(controller.decision().bitrate_kbps > 2_000);
    }

    #[test]
    fn lossless_safari_rtt_jitter_and_remb_recover_from_the_floor() {
        let now = Instant::now();
        let mut controller = RateController::new(50_000, 60);
        controller.update_sender(4_000.0, false, now - Duration::from_secs(3));
        let mut down = sample(now, 0.0);
        down.remb_kbps = Some(182);
        assert_eq!(controller.observe(down).unwrap().bitrate_kbps, 250);

        let start = now + Duration::from_secs(1);
        let mut baseline = sample(start, 0.0);
        baseline.rtt = Some(Duration::from_micros(6_317));
        controller.observe(baseline);
        let trace = [
            (157, 41, 194, 182),
            (599, 279, 170, 182),
            (1_023, 82, 147, 182),
            (1_379, 23, 141, 186),
            (2_301, 117, 125, 186),
            (2_828, 44, 112, 186),
            (3_204, 25, 97, 186),
            (3_699, 106, 106, 186),
            (4_462, 62, 94, 186),
            (5_058, 79, 98, 186),
            (5_460, 68, 102, 186),
        ];
        let mut applied = controller.decision();
        for cycle in 0..7 {
            for (offset, rtt, sent, remb) in trace {
                let at = start + Duration::from_millis(cycle * 5_600 + offset);
                controller.update_sender(f64::from(sent), false, at);
                let mut input = sample(at, 0.0);
                input.rtt = Some(Duration::from_millis(rtt));
                input.remb_kbps = Some(remb);
                if let Some(decision) = controller.observe(input) {
                    applied = decision;
                }
            }
        }
        assert!(applied.bitrate_kbps >= 4_000, "{applied:?}");
        assert_eq!(applied.fps, 60);
        assert!(controller.last_delay_cut.is_none());
    }

    #[test]
    fn persistent_delay_cuts_but_a_stale_minimum_eventually_expires() {
        let now = Instant::now();
        let mut controller = RateController::new(8_000, 60);
        for second in 0..=31 {
            let at = now + Duration::from_secs(second);
            controller.update_sender(400.0, false, at);
            let mut input = sample(at, 0.0);
            input.rtt = Some(Duration::from_millis(if second == 0 { 40 } else { 120 }));
            controller.observe(input);
            let (_, delay) = controller.last_delay.unwrap();
            if second == 1 {
                assert!(!delay.congested);
                assert!(controller.last_delay_cut.is_none());
            } else if (4..=30).contains(&second) {
                assert!(delay.congested);
                assert!(controller.last_delay_cut.is_some());
            } else if second == 31 {
                assert_eq!(delay.baseline, input.rtt);
                assert!(!delay.congested);
            }
        }
    }

    #[test]
    fn uncorroborated_remb_does_not_hold_a_lossless_probe_at_the_floor() {
        let now = Instant::now();
        let mut controller = RateController::new(8_000, 60);
        controller.update_sender(2_000.0, false, now - Duration::from_secs(3));
        let mut input = sample(now, 0.0);
        input.remb_kbps = Some(100);
        assert_eq!(controller.observe(input).unwrap().bitrate_kbps, 250);
        for seconds in 1..=20 {
            let at = now + Duration::from_secs(seconds);
            controller.update_sender(194.0, false, at);
            let mut input = sample(at, 0.0);
            input.remb_kbps = Some(182 + (seconds % 2) as u32);
            controller.observe(input);
        }
        assert!(controller.decision().bitrate_kbps > 260);
    }

    fn two_megabit_link(ceiling: u32, baseline_rtt: Duration) -> (f64, u32) {
        let now = Instant::now();
        let mut controller = RateController::new(ceiling, 60);
        let mut applied = controller.decision().bitrate_kbps;
        let mut queued_kbits = 0.0_f64;
        let mut max_queue = 0.0_f64;
        for second in 0..60 {
            let mut input = sample(now + Duration::from_secs(second), 0.0);
            input.rtt = Some(baseline_rtt + Duration::from_secs_f64(queued_kbits / 2_000.0));
            if let Some(decision) = controller.observe(input) {
                applied = decision.bitrate_kbps;
            }
            queued_kbits = (queued_kbits + f64::from(applied) - 2_000.0).max(0.0);
            max_queue = max_queue.max(queued_kbits);
        }
        (max_queue / 2_000.0, applied)
    }

    #[test]
    fn delay_feedback_bounds_queue_and_recovers_on_a_two_megabit_link() {
        let (max_queue, applied) = two_megabit_link(8_000, Duration::from_millis(80));
        assert!(max_queue < 0.5);
        assert!((1_000..=2_200).contains(&applied));
    }

    #[test]
    fn raising_the_ceiling_does_not_add_queue_on_a_two_megabit_link() {
        for rtt in [Duration::from_millis(20), Duration::from_millis(80)] {
            for ceiling in [24_000, 50_000] {
                // A larger ceiling opens at a higher estimate, so the transient backlog is
                // larger — but the delay cuts must still pull the applied rate back onto the
                // link and keep the queue bounded.
                let (max_queue, applied) = two_megabit_link(ceiling, rtt);
                assert!(
                    (1_000..=2_500).contains(&applied),
                    "ceiling={ceiling} rtt={rtt:?} applied={applied}"
                );
                assert!(max_queue < 12.0, "ceiling={ceiling} queue={max_queue}");
            }
        }
    }

    #[test]
    fn pacer_refresh_then_idle_keepalive() {
        let now = Instant::now();
        let mut pacer = DamagePacer::new(60);
        assert_eq!(pacer.on_frame(now, false, false), PaceAction::Skip);
        assert_eq!(pacer.on_frame(now, true, false), PaceAction::Encode);
        assert!(!pacer.idle());
        assert_eq!(pacer.next_deadline(now, 60), now + REFRESH_DELAY);
        assert_eq!(
            pacer.on_frame(now + Duration::from_millis(249), false, false),
            PaceAction::Skip
        );
        let refresh = now + REFRESH_DELAY;
        assert_eq!(pacer.on_frame(refresh, false, false), PaceAction::Encode);
        assert!(pacer.idle());
        assert_eq!(pacer.next_deadline(refresh, 60), refresh + IDLE_KEEPALIVE);
        assert_eq!(
            pacer.on_frame(
                refresh + IDLE_KEEPALIVE - Duration::from_millis(1),
                false,
                false
            ),
            PaceAction::Skip
        );
        assert_eq!(
            pacer.on_frame(refresh + IDLE_KEEPALIVE, false, false),
            PaceAction::Encode
        );
        assert_eq!(
            pacer.on_frame(refresh + IDLE_KEEPALIVE, false, false),
            PaceAction::Skip
        );
    }

    #[test]
    fn pacer_retains_throttled_damage_and_forces_keyframes() {
        let now = Instant::now();
        let mut pacer = DamagePacer::new(20);
        assert_eq!(pacer.on_frame(now, true, false), PaceAction::Encode);
        assert_eq!(
            pacer.on_frame(now + Duration::from_millis(10), true, false),
            PaceAction::Skip
        );
        assert_eq!(
            pacer.next_deadline(now, 20),
            now + Duration::from_millis(25)
        );
        assert_eq!(
            pacer.on_frame(now + Duration::from_millis(50), false, false),
            PaceAction::Encode
        );
        assert_eq!(
            pacer.on_frame(now + Duration::from_millis(51), false, true),
            PaceAction::Encode
        );
        assert_eq!(
            pacer.on_frame(now + Duration::from_millis(250), false, false),
            PaceAction::Skip
        );
        assert_eq!(
            pacer.on_frame(now + Duration::from_millis(260), false, false),
            PaceAction::Encode
        );
        pacer.set_fps(10);
        assert_eq!(
            pacer.on_frame(now + Duration::from_millis(300), true, false),
            PaceAction::Skip
        );
        assert_eq!(
            pacer.on_frame(now + Duration::from_millis(360), false, false),
            PaceAction::Encode
        );
    }

    #[test]
    fn first_damage_after_idle_encodes_on_arrival_without_waiting_for_refresh() {
        let now = Instant::now();
        let mut pacer = DamagePacer::new(60);
        assert_eq!(pacer.on_frame(now, true, false), PaceAction::Encode);
        assert_eq!(
            pacer.on_frame(now + REFRESH_DELAY, false, false),
            PaceAction::Encode
        );
        let arrival = now + Duration::from_secs(1);
        assert!(pacer.next_deadline(arrival, 60) > arrival);
        assert_eq!(pacer.on_frame(arrival, true, false), PaceAction::Encode);
    }

    #[test]
    fn frame_comparison_handles_empty_lengths_and_all_bytes() {
        assert!(!frame_changed(&[], &[]));
        assert!(frame_changed(&[], &[0]));
        let prev = [42; 259];
        assert!(!frame_changed(&prev, &prev));
        for index in [0, 127, 128, 258] {
            let mut next = prev;
            next[index] = 0;
            assert!(frame_changed(&prev, &next));
        }
    }

    #[test]
    fn half_interval_pacer_preserves_pending_damage_and_clamps_zero_fps() {
        let start = Instant::now();
        let mut pacer = DamagePacer::new(60);
        assert_eq!(pacer.on_frame(start, true, false), PaceAction::Encode);
        assert_eq!(
            pacer.on_frame(start + Duration::from_millis(8), true, false),
            PaceAction::Skip
        );
        assert_eq!(
            pacer.next_deadline(start, 60),
            start + Duration::from_secs_f64(0.5 / 60.0)
        );
        assert_eq!(
            pacer.on_frame(start + Duration::from_secs_f64(0.5 / 60.0), false, false),
            PaceAction::Encode
        );
        assert_eq!(
            pacer.on_frame(start + Duration::from_millis(9), false, true),
            PaceAction::Encode
        );
        let mut slow = DamagePacer::new(0);
        assert_eq!(slow.on_frame(start, false, false), PaceAction::Skip);
        assert_eq!(slow.on_frame(start, true, false), PaceAction::Encode);
        assert_eq!(slow.on_frame(start, true, false), PaceAction::Skip);
        assert_eq!(
            slow.next_deadline(start, 0),
            start + Duration::from_millis(500)
        );
    }

    #[test]
    fn capture_pacing_tolerates_callback_jitter_but_not_unpaced_bursts() {
        let start = Instant::now();
        let mut pacer = DamagePacer::new(30);
        assert_eq!(
            pacer.on_capture_frame(start, true, false),
            PaceAction::Encode
        );
        assert_eq!(
            pacer.on_capture_frame(start + Duration::from_millis(14), true, false),
            PaceAction::Skip
        );
        assert_eq!(
            pacer.on_capture_frame(start + Duration::from_millis(16), true, false),
            PaceAction::Encode
        );
        assert_eq!(
            pacer.on_capture_frame(start + Duration::from_millis(16), true, false),
            PaceAction::Skip
        );
        assert_eq!(
            pacer.on_capture_frame(start + Duration::from_millis(32), true, false),
            PaceAction::Encode
        );
    }

    #[test]
    fn capture_pacing_does_not_advance_unchanged_damage_or_idle_refresh() {
        let start = Instant::now();
        let mut pacer = DamagePacer::new(30);
        assert_eq!(
            pacer.on_capture_frame(start, true, false),
            PaceAction::Encode
        );
        assert_eq!(
            pacer.on_capture_frame(start + Duration::from_millis(10), true, false),
            PaceAction::Skip
        );
        assert_eq!(
            pacer.on_capture_frame(start + Duration::from_millis(16), false, false),
            PaceAction::Skip
        );
        assert_eq!(
            pacer.on_capture_frame(start + Duration::from_millis(17), false, false),
            PaceAction::Encode
        );
        assert_eq!(
            pacer.on_capture_frame(start + Duration::from_millis(18), false, true),
            PaceAction::Encode
        );
        assert_eq!(
            pacer.on_capture_frame(start + REFRESH_DELAY, false, false),
            PaceAction::Skip
        );
        assert_eq!(
            pacer.on_capture_frame(
                start + REFRESH_DELAY + Duration::from_millis(10),
                false,
                false
            ),
            PaceAction::Encode
        );
        assert!(pacer.idle());
    }

    #[test]
    fn capture_pacing_tracks_rate_changes_and_clamps_invalid_zero_fps() {
        let start = Instant::now();
        let mut pacer = DamagePacer::new(30);
        pacer.on_capture_frame(start, true, false);
        pacer.set_fps(10);
        assert_eq!(
            pacer.on_capture_frame(start + Duration::from_millis(47), true, false),
            PaceAction::Skip
        );
        assert_eq!(
            pacer.on_capture_frame(start + Duration::from_millis(48), true, false),
            PaceAction::Encode
        );
        pacer.set_fps(0);
        assert_eq!(
            pacer.on_capture_frame(start + Duration::from_millis(545), true, false),
            PaceAction::Skip
        );
        assert_eq!(
            pacer.on_capture_frame(start + Duration::from_millis(546), true, false),
            PaceAction::Encode
        );
    }
}
