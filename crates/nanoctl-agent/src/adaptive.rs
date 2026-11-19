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
    low_fps_since: Option<Instant>,
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
            low_fps_since: None,
        }
    }

    pub fn decision(&self) -> RateDecision {
        RateDecision {
            bitrate_kbps: self.rate.floor() as u32,
            fps: self.fps,
        }
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
        let loss = sample.fraction_lost.clamp(0.0, 1.0);
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
            let rising = trend.is_some_and(|slope| slope > 0.015);
            let draining = trend.is_some_and(|slope| slope < -0.005);
            delay_congested = rtt.as_secs_f64() > baseline + (baseline * 0.25).max(0.03) || rising;
            if delay_congested
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
        if loss > 0.1 || delay_cut {
            self.last_congestion = Some(sample.at);
            self.congestion_rate = Some(self.rate);
        }
        if loss > 0.1 {
            self.rate *= 1.0 - 0.5 * loss;
        } else if loss < 0.02 && !delay_congested {
            if let Some(last) = self.last_congestion {
                if sample.at.duration_since(last) >= Duration::from_secs(3) {
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
                self.rate *= 1.08_f64.powf(elapsed);
            }
        }
        if delay_cut {
            self.rate *= 0.85;
        }
        if let Some(remb) = sample.remb_kbps {
            self.rate = self.rate.min(remb as f64 * 0.95);
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
        if decision.fps != self.reported.fps
            || delta >= (self.reported.bitrate_kbps as f64 * 0.1).max(1.0)
        {
            self.reported = decision;
            Some(decision)
        } else {
            None
        }
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
        Duration::from_secs_f64(1.0 / fps.max(1) as f64)
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
        if changed {
            self.pending = true;
            self.refresh_pending = true;
            self.last_change = Some(now);
        }
        let ready = self
            .last_encode
            .is_none_or(|last| now.saturating_duration_since(last) >= Self::interval(self.fps));
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
        let mut input = sample(now, 0.05);
        input.remb_kbps = Some(1_001);
        assert_eq!(controller.observe(input).unwrap().bitrate_kbps, 950);
        input.remb_kbps = Some(1_000);
        assert_eq!(controller.observe(input), None);
        input.remb_kbps = Some(0);
        assert_eq!(controller.observe(input).unwrap().bitrate_kbps, 250);
        input.remb_kbps = None;
        input.fraction_lost = 0.0;
        input.at += Duration::from_secs(1_000);
        assert_eq!(controller.observe(input).unwrap().bitrate_kbps, 8_000);
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
        input.at += Duration::from_secs(4);
        controller.observe(input);
        assert!(controller.decision().bitrate_kbps > 2_500);
        assert_eq!(controller.decision().fps, 30);
        input.at += Duration::from_secs(3);
        assert_eq!(controller.observe(input).unwrap().fps, 60);
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
            now + Duration::from_millis(50)
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
}
