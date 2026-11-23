use std::collections::VecDeque;
use std::time::{Duration, Instant};

const RTT_WINDOW: Duration = Duration::from_secs(30);
const REFRESH_DELAY: Duration = Duration::from_millis(250);
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

pub struct RateController {
    ceiling: u32,
    max_fps: u16,
    rate: f64,
    fps: u16,
    reported: RateDecision,
    last_sample: Option<Instant>,
    rtts: VecDeque<(Instant, Duration)>,
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
        let initial = (ceiling_kbps / 4)
            .min(4_000)
            .clamp(ceiling_kbps.min(250), ceiling_kbps);
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
        if rate >= 2_500.0 {
            max_fps
        } else if rate >= 1_000.0 {
            max_fps.min(30)
        } else if rate >= 500.0 {
            max_fps.min(20)
        } else {
            max_fps.min(15)
        }
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
        let fresh_report = sample
            .receiver_report_at
            .is_some_and(|at| self.last_loss_report.is_none_or(|last| at > last));
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
        let mut baseline_ms = None;
        let mut trend_ms_per_s = None;
        let mut trend_rising = false;
        let mut remb_cut = false;
        let mut queued = false;
        let mut delay_congested = false;
        let mut delay_cut = false;
        while self
            .rtts
            .front()
            .is_some_and(|(at, _)| sample.at.duration_since(*at) > RTT_WINDOW)
        {
            self.rtts.pop_front();
        }
        if let Some(rtt) = sample.rtt {
            let trend = self.rtts.back().and_then(|(at, previous)| {
                let seconds = sample.at.duration_since(*at).as_secs_f64();
                (seconds >= 0.1).then(|| (rtt.as_secs_f64() - previous.as_secs_f64()) / seconds)
            });
            self.rtts.push_back((sample.at, rtt));
            let baseline = self
                .rtts
                .iter()
                .map(|(_, rtt)| rtt.as_secs_f64())
                .fold(f64::INFINITY, f64::min);
            queued = rtt.as_secs_f64() - baseline >= 0.015;
            let rising = queued && trend.is_some_and(|slope| slope > 0.015);
            baseline_ms = Some(baseline * 1000.0);
            trend_ms_per_s = trend.map(|slope| slope * 1000.0);
            trend_rising = rising;
            let draining = trend.is_some_and(|slope| slope < -0.005);
            delay_congested = rtt.as_secs_f64() > baseline + (baseline * 0.25).max(0.03) || rising;
            if delay_congested
                && !self.app_limited
                && !draining
                && self.last_delay_cut.is_none_or(|last| {
                    sample.at.duration_since(last)
                        >= rtt.clamp(Duration::from_millis(300), Duration::from_secs(1))
                })
            {
                delay_cut = true;
                self.last_delay_cut = Some(sample.at);
            }
        }
        if loss < 0.02 && !delay_congested && sample.rtt.is_some() && !self.app_limited {
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
                    let gain = if sample
                        .rtt
                        .is_some_and(|rtt| rtt <= Duration::from_millis(10))
                    {
                        2.0_f64
                    } else {
                        Self::probe_gain(sample.rtt)
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
                let startup = self.rate < f64::from(self.ceiling) * 0.75
                    && self.clean_since.is_some_and(|since| {
                        sample.at.duration_since(since) >= Duration::from_secs(3)
                    });
                self.rate *= if startup {
                    Self::probe_gain(sample.rtt).powf(elapsed.min(2.0))
                } else {
                    1.08_f64.powf(elapsed)
                };
            }
        }
        if delay_cut {
            self.rate *= 0.85;
        }
        if let Some(remb) = new_remb
            && (sample.rtt.is_none() || queued || sustained_loss)
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
        self.rate = self
            .rate
            .clamp(self.ceiling.min(250) as f64, self.ceiling as f64);

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
            || delta >= (self.reported.bitrate_kbps as f64 * 0.1).max(1.0);
        if changed || loss_cut || delay_cut || remb_cut {
            let reason = if remb_cut {
                "remb"
            } else if loss_cut {
                "loss"
            } else if delay_cut && trend_rising {
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
                remb_corroborated = sample.rtt.is_none() || queued || sustained_loss,
                rtt_ms = sample.rtt.map(|rtt| rtt.as_secs_f64() * 1000.0),
                baseline_ms,
                trend_ms_per_s,
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
        rtt.map_or(1.08, |rtt| {
            (1.0 + 0.004 / rtt.as_secs_f64().max(0.001)).clamp(1.08, 1.5)
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
    fn delay_cut_is_throttled_and_baseline_expires() {
        let now = Instant::now();
        let mut controller = RateController::new(8_000, 60);
        let mut input = sample(now, 0.05);
        input.rtt = Some(Duration::from_millis(40));
        controller.observe(input);
        input.at += Duration::from_millis(100);
        input.rtt = Some(Duration::from_millis(120));
        assert_eq!(controller.observe(input).unwrap().bitrate_kbps, 1_700);
        input.at += Duration::from_millis(100);
        assert_eq!(controller.observe(input), None);
        input.at += Duration::from_millis(200);
        assert_eq!(controller.observe(input).unwrap().bitrate_kbps, 1_445);
        input.at = now + Duration::from_secs(31);
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
        input.remb_kbps = Some(2_000);
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
            controller.observe(input);
        }
        assert!(controller.decision().bitrate_kbps > 2_500);
        assert_eq!(controller.decision().fps, 30);
        for second in 9..=10 {
            input.at = now + Duration::from_secs(second);
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
            controller.update_sender(4_000.0, false, input.at);
            controller.observe(input);
        }
        assert_eq!(controller.decision().bitrate_kbps, 8_000);
        input.remb_kbps = Some(1_500);
        input.at += Duration::from_secs(1);
        input.rtt = Some(Duration::from_millis(25));
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
        input.rtt = Some(Duration::from_millis(25));
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
        input.rtt = Some(Duration::from_millis(120));
        assert_eq!(controller.observe(input).unwrap().bitrate_kbps, 1_700);
        input.at += Duration::from_millis(100);
        assert_eq!(controller.observe(input), None);
        assert_eq!(controller.decision().bitrate_kbps, 1_700);
    }

    #[test]
    fn delay_trend_cuts_before_absolute_threshold_and_drain_does_not_recursively_cut() {
        let now = Instant::now();
        let mut controller = RateController::new(8_000, 60);
        let mut input = sample(now, 0.0);
        input.rtt = Some(Duration::from_millis(80));
        controller.observe(input);
        input.at += Duration::from_secs(1);
        input.rtt = Some(Duration::from_millis(100));
        assert_eq!(controller.observe(input).unwrap().bitrate_kbps, 1_700);
        input.at += Duration::from_secs(1);
        input.rtt = Some(Duration::from_millis(200));
        assert_eq!(controller.observe(input).unwrap().bitrate_kbps, 1_445);
        input.at += Duration::from_secs(1);
        input.rtt = Some(Duration::from_millis(160));
        assert_eq!(controller.observe(input), None);
        assert_eq!(controller.decision().bitrate_kbps, 1_445);
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
    fn delay_feedback_bounds_queue_and_recovers_on_a_two_megabit_link() {
        let now = Instant::now();
        let mut controller = RateController::new(8_000, 60);
        let mut applied = controller.decision().bitrate_kbps;
        let mut queued_kbits = 0.0_f64;
        let mut max_queue = 0.0_f64;
        for second in 0..60 {
            let mut input = sample(now + Duration::from_secs(second), 0.0);
            input.rtt = Some(Duration::from_secs_f64(0.08 + queued_kbits / 2_000.0));
            if let Some(decision) = controller.observe(input) {
                applied = decision.bitrate_kbps;
            }
            queued_kbits = (queued_kbits + f64::from(applied) - 2_000.0).max(0.0);
            max_queue = max_queue.max(queued_kbits);
        }
        assert!(max_queue / 2_000.0 < 0.5);
        assert!((1_000..=2_200).contains(&applied));
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
