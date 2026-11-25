use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(any(target_os = "linux", target_os = "windows"))]
use anyhow::Context;
use anyhow::Result;
use tokio::sync::{mpsc, watch};
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::rtp::header::Header;
use webrtc::rtp::packet::Packet;
use webrtc::track::track_local::TrackLocalWriter;
use webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP;

const SAMPLES: usize = 960 * 2;
const FRAME_DURATION: Duration = Duration::from_millis(20);

#[cfg(not(target_os = "windows"))]
pub fn available() -> bool {
    #[cfg(target_os = "linux")]
    {
        use std::process::{Command, Stdio};
        let probe = |program: &str, argument: &str| {
            Command::new(program)
                .arg(argument)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        };
        probe("parec", "--version") && probe("pactl", "info")
    }
    #[cfg(target_os = "macos")]
    {
        false
    }
}

#[cfg(target_os = "windows")]
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioOutput {
    pub id: String,
    name: String,
    pub is_default: bool,
}

#[cfg(target_os = "windows")]
pub fn outputs() -> Vec<AudioOutput> {
    let Some(worker) = windows_worker() else {
        return Vec::new();
    };
    let (ready, result) = std::sync::mpsc::sync_channel(1);
    if worker.commands.send(WindowsCommand::Probe(ready)).is_err() {
        return Vec::new();
    }
    result.recv().unwrap_or_default()
}

#[cfg(target_os = "windows")]
fn effective_output<'a>(outputs: &'a [AudioOutput], selected: Option<&str>) -> Option<&'a str> {
    outputs
        .iter()
        .find(|output| selected.map_or(output.is_default, |id| id == output.id))
        .map(|output| output.id.as_str())
}

#[cfg(target_os = "windows")]
#[derive(Default)]
pub struct OutputSelection {
    pub outputs: Vec<AudioOutput>,
    pub selected: Option<String>,
}

#[cfg(target_os = "windows")]
impl OutputSelection {
    pub fn refresh(&mut self, outputs: Vec<AudioOutput>) {
        self.outputs = outputs;
        if self.selected.is_some() && self.effective().is_none() {
            self.selected = None;
        }
    }

    pub fn select(&mut self, requested: Option<&str>) {
        if requested.is_none() || effective_output(&self.outputs, requested).is_some() {
            self.selected = requested.map(str::to_owned);
        }
    }

    pub fn effective(&self) -> Option<String> {
        effective_output(&self.outputs, self.selected.as_deref()).map(str::to_owned)
    }
}

fn encoder() -> Result<opus::Encoder> {
    encoder_with_bitrate(128)
}

fn encoder_with_bitrate(kbps: u32) -> Result<opus::Encoder> {
    let mut encoder =
        opus::Encoder::new(48_000, opus::Channels::Stereo, opus::Application::LowDelay)?;
    encoder.set_bitrate(opus::Bitrate::Bits(kbps.saturating_mul(1_000) as i32))?;
    // Discontinuous transmission: silence sends a tiny comfort-noise frame
    // instead of a full 128kbps encode. A muted desktop stops paying for
    // audio it never had.
    encoder.set_dtx(true)?;
    Ok(encoder)
}

/// Audio is a constant share of the same association the video congestion
/// controller is measuring. Keep full quality while the link has comfortable
/// headroom, but spend scarce capacity on video first: below these REMB tiers
/// 128kbps stereo audio would otherwise crowd out the pictures the input is
/// reacting to. Opus bitrate changes are in-band and need no decoder reset.
#[derive(Debug)]
struct AdaptiveAudioBitrate {
    bitrate_kbps: u32,
    raise_since: Option<Instant>,
}

impl Default for AdaptiveAudioBitrate {
    fn default() -> Self {
        Self {
            bitrate_kbps: 128,
            raise_since: None,
        }
    }
}

impl AdaptiveAudioBitrate {
    const RAISE_DELAY: Duration = Duration::from_secs(5);

    fn target(sample: crate::adaptive::LinkSample) -> u32 {
        match sample.remb_kbps.unwrap_or(u32::MAX) {
            2_500.. => 128,
            1_500.. => 96,
            700.. => 64,
            _ => 48,
        }
    }

    fn observe(&mut self, sample: crate::adaptive::LinkSample) -> u32 {
        let target = Self::target(sample);
        if target < self.bitrate_kbps {
            self.bitrate_kbps = target;
            self.raise_since = None;
        } else if target > self.bitrate_kbps {
            let since = *self.raise_since.get_or_insert(sample.at);
            if sample.at.duration_since(since) >= Self::RAISE_DELAY {
                self.bitrate_kbps = target;
                self.raise_since = None;
            }
        } else {
            self.raise_since = None;
        }
        self.bitrate_kbps
    }
}

#[cfg(any(target_os = "linux", test))]
#[derive(Default)]
struct PcmFramer {
    bytes: Vec<u8>,
}

#[cfg(any(target_os = "linux", test))]
impl PcmFramer {
    fn push(&mut self, bytes: &[u8]) -> Vec<Vec<i16>> {
        self.bytes.extend_from_slice(bytes);
        let complete = self.bytes.len() / (SAMPLES * 2) * (SAMPLES * 2);
        let frames = self.bytes[..complete]
            .chunks_exact(SAMPLES * 2)
            .map(|frame| {
                frame
                    .chunks_exact(2)
                    .map(|sample| i16::from_le_bytes([sample[0], sample[1]]))
                    .collect()
            })
            .collect();
        self.bytes.drain(..complete);
        frames
    }
}

#[cfg(target_os = "linux")]
fn capture_command() -> tokio::process::Command {
    let mut command = tokio::process::Command::new("parec");
    command.args([
        "--device=@DEFAULT_MONITOR@",
        "--raw",
        "--format=s16le",
        "--rate=48000",
        "--channels=2",
        "--latency-msec=10",
    ]);
    command
}

#[cfg(target_os = "linux")]
struct Capture {
    child: tokio::process::Child,
    reader: tokio::task::JoinHandle<()>,
}

#[cfg(target_os = "linux")]
impl Drop for Capture {
    fn drop(&mut self) {
        self.reader.abort();
        let _ = self.child.start_kill();
    }
}

#[cfg(target_os = "linux")]
fn capture() -> Result<(Capture, mpsc::Receiver<Vec<i16>>)> {
    use std::process::Stdio;
    use tokio::io::AsyncReadExt;
    let mut child = capture_command()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("starting parec")?;
    let mut stdout = child.stdout.take().context("parec stdout")?;
    let (sender, receiver) = mpsc::channel(3);
    let reader = tokio::spawn(async move {
        let mut bytes = [0; SAMPLES * 2];
        let mut framer = PcmFramer::default();
        while let Ok(count) = stdout.read(&mut bytes).await {
            if count == 0 {
                break;
            }
            for frame in framer.push(&bytes[..count]) {
                if sender.is_closed() {
                    return;
                }
                let _ = sender.try_send(frame);
            }
        }
    });
    Ok((Capture { child, reader }, receiver))
}

#[cfg(target_os = "windows")]
struct Capture {
    commands: std::sync::mpsc::Sender<WindowsCommand>,
    id: u64,
    failed: tokio::sync::oneshot::Receiver<()>,
}

#[cfg(target_os = "windows")]
impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.commands.send(WindowsCommand::Stop(self.id));
    }
}

#[cfg(target_os = "windows")]
#[derive(Debug)]
enum WindowsCommand {
    Probe(std::sync::mpsc::SyncSender<Vec<AudioOutput>>),
    Start {
        output: u64,
        pcm: mpsc::Sender<Vec<i16>>,
        failed: tokio::sync::oneshot::Sender<()>,
        ready: std::sync::mpsc::SyncSender<std::result::Result<u64, String>>,
    },
    Stop(u64),
}

#[cfg(target_os = "windows")]
struct WindowsWorker {
    commands: std::sync::mpsc::Sender<WindowsCommand>,
}

#[cfg(target_os = "windows")]
fn windows_worker() -> Option<&'static WindowsWorker> {
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

    static WORKER: std::sync::OnceLock<Option<WindowsWorker>> = std::sync::OnceLock::new();
    WORKER
        .get_or_init(|| {
            let (commands, requests) = std::sync::mpsc::channel();
            let (ready, started) = std::sync::mpsc::sync_channel(1);
            std::thread::Builder::new()
                .name("audio-loopback".into())
                .spawn(move || {
                    // SAFETY: this thread owns its MTA and all CPAL objects created below.
                    if let Err(error) =
                        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok()
                    {
                        tracing::warn!(%error, "cannot initialize Windows audio COM apartment");
                        return;
                    }
                    struct Apartment;
                    impl Drop for Apartment {
                        fn drop(&mut self) {
                            // SAFETY: paired with successful initialization on this thread.
                            unsafe { CoUninitialize() };
                        }
                    }
                    let _apartment = Apartment;
                    if ready.send(()).is_err() {
                        return;
                    }
                    let mut streams = std::collections::HashMap::new();
                    let mut next_id = 0_u64;
                    let mut endpoints = WindowsOutputs::default();
                    while let Ok(command) = requests.recv() {
                        match command {
                            WindowsCommand::Probe(ready) => {
                                let _ = ready.send(endpoints.refresh().unwrap_or_else(|error| {
                                    tracing::debug!(%error, "cannot enumerate Windows audio outputs");
                                    Vec::new()
                                }));
                            }
                            WindowsCommand::Start { output, pcm, failed, ready } => {
                                let result = endpoints
                                    .devices
                                    .iter()
                                    .find(|(id, _)| *id == output)
                                    .map(|(_, device)| device)
                                    .context("selected Windows audio output is unavailable")
                                    .and_then(|device| windows_capture(device, pcm, failed))
                                    .map(|stream| {
                                    let id = next_id;
                                    next_id += 1;
                                    streams.insert(id, stream);
                                    id
                                });
                                if let Err(error) =
                                    ready.send(result.map_err(|error| error.to_string()))
                                    && let Ok(id) = error.0
                                {
                                    streams.remove(&id);
                                }
                            }
                            WindowsCommand::Stop(id) => {
                                streams.remove(&id);
                            }
                        }
                    }
                })
                .ok()?;
            started.recv().ok()?;
            Some(WindowsWorker { commands })
        })
        .as_ref()
}

#[cfg(target_os = "windows")]
#[derive(Default)]
struct WindowsOutputs {
    devices: Vec<(u64, cpal::platform::WasapiDevice)>,
    next_id: u64,
}

#[cfg(target_os = "windows")]
impl WindowsOutputs {
    fn refresh(&mut self) -> Result<Vec<AudioOutput>> {
        use cpal::traits::HostTrait;

        let host = cpal::platform::WasapiHost::new()?;
        let default = host.default_output_device();
        let mut devices = Vec::new();
        for device in host.output_devices()? {
            let id = self
                .devices
                .iter()
                .find_map(|(id, previous)| (previous == &device).then_some(*id))
                .unwrap_or_else(|| {
                    self.next_id += 1;
                    self.next_id
                });
            devices.push((id, device));
        }
        devices.sort_unstable_by_key(|(id, _)| *id);
        let outputs = devices
            .iter()
            .map(|(id, device)| AudioOutput {
                id: id.to_string(),
                name: device.name().unwrap_or_else(|_| format!("Output {id}")),
                is_default: default.as_ref() == Some(device),
            })
            .collect();
        self.devices = devices;
        Ok(outputs)
    }
}

#[cfg(target_os = "windows")]
fn capture(output: &str) -> Result<(Capture, mpsc::Receiver<Vec<i16>>)> {
    let worker = windows_worker().context("cannot initialize audio worker")?;
    let (pcm, receiver) = mpsc::channel(3);
    let (ready, started) = std::sync::mpsc::sync_channel(1);
    let (failed, failure) = tokio::sync::oneshot::channel();
    worker.commands.send(WindowsCommand::Start {
        output: output.parse()?,
        pcm,
        failed,
        ready,
    })?;
    let id = started.recv()?.map_err(anyhow::Error::msg)?;
    Ok((
        Capture {
            commands: worker.commands.clone(),
            id,
            failed: failure,
        },
        receiver,
    ))
}

#[cfg(target_os = "windows")]
fn windows_capture(
    device: &cpal::platform::WasapiDevice,
    sender: mpsc::Sender<Vec<i16>>,
    failed: tokio::sync::oneshot::Sender<()>,
) -> Result<cpal::platform::WasapiStream> {
    use cpal::traits::StreamTrait;
    let config = device
        .default_output_config()
        .context("reading Windows output mix format")?;
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => windows_stream::<f32>(device, &config.into(), sender, failed),
        cpal::SampleFormat::I16 => windows_stream::<i16>(device, &config.into(), sender, failed),
        cpal::SampleFormat::U16 => windows_stream::<u16>(device, &config.into(), sender, failed),
        cpal::SampleFormat::U8 => windows_stream::<u8>(device, &config.into(), sender, failed),
        cpal::SampleFormat::I32 => windows_stream::<i32>(device, &config.into(), sender, failed),
        cpal::SampleFormat::I64 => windows_stream::<i64>(device, &config.into(), sender, failed),
        format => anyhow::bail!("unsupported loopback format {format:?}"),
    }?;
    stream.play()?;
    Ok(stream)
}

#[cfg(target_os = "windows")]
fn windows_stream<T>(
    device: &cpal::platform::WasapiDevice,
    config: &cpal::StreamConfig,
    sender: mpsc::Sender<Vec<i16>>,
    failed: tokio::sync::oneshot::Sender<()>,
) -> Result<cpal::platform::WasapiStream>
where
    T: cpal::SizedSample + cpal::Sample,
    f32: cpal::FromSample<T>,
{
    use cpal::traits::DeviceTrait;
    let channels = usize::from(config.channels);
    let rate = u64::from(config.sample_rate.0);
    let mut phase = 0_u64;
    let mut frame = Vec::with_capacity(SAMPLES);
    let mut failed = Some(failed);
    let mut mmcss = None;
    Ok(device.build_input_stream(
        config,
        move |data: &[T], _| {
            // WASAPI invokes this callback on its realtime capture thread. Register
            // that thread with MMCSS once rather than the control thread that built
            // the stream; the guard reverts when the stream is dropped.
            if mmcss.is_none() {
                mmcss = crate::media::register_mmcss();
            }
            for input in data.chunks_exact(channels) {
                phase += 48_000;
                while phase >= rate {
                    phase -= rate;
                    for channel in 0..2 {
                        let sample: f32 = input[channel.min(channels - 1)].to_sample();
                        frame.push((sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16);
                    }
                    if frame.len() == SAMPLES {
                        let _ = sender
                            .try_send(std::mem::replace(&mut frame, Vec::with_capacity(SAMPLES)));
                    }
                }
            }
        },
        move |error| {
            tracing::warn!(%error, "loopback capture failed");
            if let Some(failed) = failed.take() {
                let _ = failed.send(());
            }
        },
        None,
    )?)
}

#[cfg(target_os = "macos")]
fn capture() -> Result<((), mpsc::Receiver<Vec<i16>>)> {
    anyhow::bail!("system audio is unsupported on macOS")
}

pub async fn run(
    track: Arc<TrackLocalStaticRTP>,
    peer: Arc<RTCPeerConnection>,
    mut shutdown: watch::Receiver<bool>,
    output: watch::Receiver<Option<String>>,
    enabled: watch::Receiver<bool>,
    link_feedback: watch::Receiver<Option<crate::adaptive::LinkSample>>,
) {
    if *shutdown.borrow() {
        return;
    }
    while peer.connection_state() != RTCPeerConnectionState::Connected {
        tokio::select! {
            _ = shutdown.changed() => return,
            _ = tokio::time::sleep(FRAME_DURATION) => {}
        }
    }
    #[cfg(not(target_os = "windows"))]
    let _ = output;
    #[cfg(not(target_os = "windows"))]
    match capture() {
        Ok((_capture, pcm)) => {
            tokio::select! {
                _ = shutdown.changed() => {},
                _ = stream(&track, pcm, enabled.clone(), link_feedback.clone()) => {},
            }
        }
        Err(error) => tracing::warn!(%error, "system audio stopped"),
    }

    #[cfg(target_os = "windows")]
    let mut output = output;
    #[cfg(target_os = "windows")]
    loop {
        let selected = output.borrow_and_update().clone();
        if let Some(selected) = selected {
            match capture(&selected) {
                Ok((mut capture, pcm)) => {
                    tokio::select! {
                        _ = shutdown.changed() => return,
                        result = output.changed() => {
                            if result.is_err() { return; }
                            continue;
                        },
                        _ = stream(&track, pcm, enabled.clone(), link_feedback.clone()) => {},
                        _ = &mut capture.failed => {},
                    }
                }
                Err(error) => tracing::warn!(%error, "cannot start Windows loopback capture"),
            }
            tokio::select! {
                _ = shutdown.changed() => return,
                result = output.changed() => if result.is_err() { return; },
                _ = tokio::time::sleep(Duration::from_secs(2)) => {},
            }
        } else {
            tokio::select! {
                _ = shutdown.changed() => return,
                result = output.changed() => if result.is_err() { return; },
            }
        }
    }
}

async fn stream(
    track: &TrackLocalStaticRTP,
    pcm: mpsc::Receiver<Vec<i16>>,
    enabled: watch::Receiver<bool>,
    link_feedback: watch::Receiver<Option<crate::adaptive::LinkSample>>,
) {
    if let Err(error) = stream_inner(track, pcm, enabled, link_feedback).await {
        tracing::warn!(%error, "system audio stopped");
    }
}

async fn stream_inner(
    track: &TrackLocalStaticRTP,
    mut pcm: mpsc::Receiver<Vec<i16>>,
    enabled: watch::Receiver<bool>,
    link_feedback: watch::Receiver<Option<crate::adaptive::LinkSample>>,
) -> Result<()> {
    let mut encoder = encoder()?;
    let (encoded, mut packets) = mpsc::channel(2);
    let encode_enabled = enabled.clone();
    tokio::task::spawn_blocking(move || {
        #[cfg(target_os = "windows")]
        let _mmcss = crate::media::register_mmcss();
        let mut output = [0; 4000];
        let mut adaptive = AdaptiveAudioBitrate::default();
        let mut encoder_kbps = 128;
        while let Some(frame) = pcm.blocking_recv() {
            if encoded.is_closed() {
                break;
            }
            if frame.len() != SAMPLES {
                continue;
            }
            if !*encode_enabled.borrow() {
                continue;
            }
            if let Some(sample) = *link_feedback.borrow() {
                let target = adaptive.observe(sample);
                if target != encoder_kbps {
                    match encoder.set_bitrate(opus::Bitrate::Bits(target as i32 * 1_000)) {
                        Ok(()) => encoder_kbps = target,
                        Err(error) => tracing::warn!(%error, target, "Opus bitrate update failed"),
                    }
                }
            }
            match encoder.encode(&frame, &mut output) {
                Ok(size) => {
                    let _ = encoded.try_send(output[..size].to_vec());
                }
                Err(error) => {
                    tracing::warn!(%error, "Opus encode failed");
                    break;
                }
            }
        }
    });
    let mut rtp = OpusRtp::default();
    while let Some(packet) = packets.recv().await {
        if !*enabled.borrow() {
            continue;
        }
        if let Some(packet) = rtp.packet(packet) {
            let _ = tokio::time::timeout(FRAME_DURATION * 2, track.write_rtp(&packet)).await;
        }
    }
    Ok(())
}

/// Opus samples per 20 ms frame at the 48 kHz RTP clock.
const RTP_FRAME_TICKS: u32 = 960;

/// Turns encoded Opus frames into RTP packets, dropping DTX frames.
///
/// With DTX on, libopus emits 1-2 byte TOC-only frames for every 20 ms of
/// silence and a real comfort-noise update every 400 ms. Sending the tiny
/// frames still costs a full IP/UDP/RTP/SRTP header 50 times a second, so,
/// like libwebrtc, they are not sent: the timestamp keeps advancing, the
/// sequence number does not (receivers see a gap in time, not loss), and the
/// first packet after a gap carries the marker bit to start a talkspurt.
#[derive(Default)]
struct OpusRtp {
    sequence_number: u16,
    timestamp: u32,
    in_dtx: bool,
}

impl OpusRtp {
    fn packet(&mut self, payload: Vec<u8>) -> Option<Packet> {
        let timestamp = self.timestamp;
        self.timestamp = self.timestamp.wrapping_add(RTP_FRAME_TICKS);
        if payload.len() <= 2 {
            self.in_dtx = true;
            return None;
        }
        let marker = std::mem::take(&mut self.in_dtx);
        let sequence_number = self.sequence_number;
        self.sequence_number = self.sequence_number.wrapping_add(1);
        Some(Packet {
            header: Header {
                version: 2,
                marker,
                sequence_number,
                timestamp,
                ..Default::default()
            },
            payload: payload.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_worker_probes_from_multiple_calling_threads() {
        let worker = windows_worker().expect("audio worker initializes COM");
        let expected = outputs();
        let probes: Vec<_> = (0..16).map(|_| std::thread::spawn(outputs)).collect();
        for probe in probes {
            assert_eq!(probe.join().unwrap(), expected);
        }
        worker
            .commands
            .send(WindowsCommand::Stop(u64::MAX))
            .unwrap();
        if expected.is_empty() {
            assert!(capture(&u64::MAX.to_string()).is_err());
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn selection_follows_default_unless_an_active_output_is_pinned() {
        let output = |id: &str, is_default| AudioOutput {
            id: id.into(),
            name: id.into(),
            is_default,
        };
        let mut selection = OutputSelection::default();
        assert_eq!(selection.effective(), None);
        selection.refresh(vec![output("speakers", false), output("headphones", false)]);
        assert_eq!(selection.effective(), None);
        selection.select(Some("headphones"));
        selection.select(Some("invalid"));
        assert_eq!(selection.effective().as_deref(), Some("headphones"));
        selection.refresh(vec![output("speakers", true), output("headphones", false)]);
        assert_eq!(selection.effective().as_deref(), Some("headphones"));
        selection.select(None);
        assert_eq!(selection.effective().as_deref(), Some("speakers"));
        selection.refresh(vec![output("speakers", false), output("headphones", true)]);
        assert_eq!(selection.effective().as_deref(), Some("headphones"));
        selection.select(Some("speakers"));
        selection.refresh(vec![output("headphones", true)]);
        assert_eq!(selection.selected, None);
        assert_eq!(selection.effective().as_deref(), Some("headphones"));
    }

    #[test]
    fn frames_arbitrary_pcm_chunks_without_losing_bytes() {
        let samples: Vec<i16> = (0..SAMPLES * 3 + 5).map(|index| index as i16).collect();
        let bytes: Vec<u8> = samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        let mut framer = PcmFramer::default();
        assert!(framer.push(&[]).is_empty());
        let frames: Vec<Vec<i16>> = bytes
            .chunks(137)
            .flat_map(|chunk| framer.push(chunk))
            .collect();
        assert_eq!(frames.len(), 3);
        assert_eq!(frames.concat(), samples[..SAMPLES * 3]);
        assert_eq!(framer.bytes.len(), 10);
    }

    #[test]
    fn dtx_frames_are_not_sent_but_keep_time() {
        let mut rtp = OpusRtp::default();
        let first = rtp.packet(vec![1; 40]).unwrap();
        assert_eq!(
            (first.header.sequence_number, first.header.timestamp),
            (0, 0)
        );
        assert!(!first.header.marker);
        assert!(rtp.packet(vec![8]).is_none());
        assert!(rtp.packet(vec![8, 0]).is_none());
        let resumed = rtp.packet(vec![1; 3]).unwrap();
        assert_eq!(resumed.header.sequence_number, 1);
        assert_eq!(resumed.header.timestamp, 3 * RTP_FRAME_TICKS);
        assert!(resumed.header.marker);
        assert!(!rtp.packet(vec![1; 40]).unwrap().header.marker);
    }

    #[test]
    fn adaptive_audio_bitrate_drops_fast_and_recovers_only_after_headroom() {
        let start = Instant::now();
        let sample = |remb_kbps, seconds| crate::adaptive::LinkSample {
            fraction_lost: 0.0,
            rtt: None,
            remb_kbps: Some(remb_kbps),
            at: start + Duration::from_secs(seconds),
            receiver_report_at: None,
        };
        let mut adaptive = AdaptiveAudioBitrate::default();
        assert_eq!(adaptive.observe(sample(4_000, 0)), 128);
        assert_eq!(adaptive.observe(sample(800, 1)), 64);
        assert_eq!(adaptive.observe(sample(4_000, 2)), 64);
        assert_eq!(adaptive.observe(sample(4_000, 6)), 64);
        assert_eq!(adaptive.observe(sample(4_000, 7)), 128);
    }

    /// Payload + RTP wire rate for adaptive Opus bitrate tiers on loud,
    /// wideband game-like audio. Run with `--ignored --nocapture`.
    #[test]
    #[ignore = "benchmark"]
    fn bench_adaptive_audio_wire_bytes() {
        const OVERHEAD: usize = 20 + 8 + 12 + 10;
        const SECONDS: usize = 20;
        let frames = SECONDS * 50;
        let noise = |frame: usize, state: &mut u32| -> Vec<i16> {
            (0..SAMPLES / 2)
                .flat_map(|i| {
                    *state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    let noise = ((*state >> 16) as i16).wrapping_div(3);
                    let t = (frame * SAMPLES / 2 + i) as f64 / 48_000.0;
                    let tone = (2_000.0 * (t * 220.0 * std::f64::consts::TAU).sin()) as i16;
                    let left = noise.saturating_add(tone);
                    let right = noise.saturating_add((tone as i32 * 4 / 5) as i16);
                    [left, right]
                })
                .collect()
        };
        let mut output = [0; 4000];
        for kbps in [128, 96, 64, 48] {
            let mut encoder = encoder_with_bitrate(kbps).unwrap();
            let mut local_state = 1_u32;
            let mut payload = 0;
            for frame in 0..frames {
                payload += encoder
                    .encode(&noise(frame, &mut local_state), &mut output)
                    .unwrap();
            }
            let wire = payload + frames * OVERHEAD;
            println!(
                "{kbps}kbps target: payload {:.1} kbps, wire {:.1} kbps",
                payload as f64 * 8.0 / SECONDS as f64 / 1000.0,
                wire as f64 * 8.0 / SECONDS as f64 / 1000.0,
            );
        }
    }

    /// Wire bytes per second for synthetic system-audio scenes, before and
    /// after DTX gating. Run with `--ignored --nocapture`.
    #[test]
    #[ignore = "benchmark"]
    fn bench_audio_wire_bytes() {
        // IPv4 + UDP + RTP + SRTP auth tag.
        const OVERHEAD: usize = 20 + 8 + 12 + 10;
        const SECONDS: usize = 30;
        type Scene = Box<dyn Fn(usize) -> Vec<i16>>;
        let frames = SECONDS * 50;
        let tone = |frame: usize, amplitude: f64| -> Vec<i16> {
            (0..SAMPLES / 2)
                .flat_map(|i| {
                    let t = (frame * SAMPLES / 2 + i) as f64 / 48_000.0;
                    let v = (amplitude * (t * 440.0 * std::f64::consts::TAU).sin()) as i16;
                    [v, v]
                })
                .collect()
        };
        let scenes: [(&str, Scene); 4] = [
            ("silent desktop", Box::new(|_| vec![0; SAMPLES])),
            (
                "game: 2s sound / 3s silence",
                Box::new(move |f| {
                    if f % 250 < 100 {
                        tone(f, 8_000.0)
                    } else {
                        vec![0; SAMPLES]
                    }
                }),
            ),
            (
                "notification ping every 10s",
                Box::new(move |f| {
                    if f % 500 < 15 {
                        tone(f, 8_000.0)
                    } else {
                        vec![0; SAMPLES]
                    }
                }),
            ),
            ("continuous tone", Box::new(move |f| tone(f, 8_000.0))),
        ];
        for (name, scene) in scenes {
            let mut encoder = encoder().unwrap();
            let mut output = [0; 4000];
            let mut rtp = OpusRtp::default();
            let (mut before, mut after, mut before_packets, mut after_packets) = (0, 0, 0, 0);
            for frame in 0..frames {
                let size = encoder.encode(&scene(frame), &mut output).unwrap();
                before += size + OVERHEAD;
                before_packets += 1;
                if let Some(packet) = rtp.packet(output[..size].to_vec()) {
                    after += packet.payload.len() + OVERHEAD;
                    after_packets += 1;
                }
            }
            println!(
                "{name}: {:.2} -> {:.2} kbps ({:+.1}%), {:.1} -> {:.1} pkt/s",
                before as f64 * 8.0 / SECONDS as f64 / 1000.0,
                after as f64 * 8.0 / SECONDS as f64 / 1000.0,
                (after as f64 / before as f64 - 1.0) * 100.0,
                before_packets as f64 / SECONDS as f64,
                after_packets as f64 / SECONDS as f64,
            );
        }
    }

    #[test]
    fn encodes_silence_as_opus() {
        let mut output = [0; 4000];
        assert!(
            encoder()
                .unwrap()
                .encode(&[0; SAMPLES], &mut output)
                .unwrap()
                > 0
        );
        assert!(encoder().unwrap().encode(&[0; 7], &mut output).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn command_captures_default_sink_not_microphone() {
        let command = capture_command();
        let args: Vec<_> = command
            .as_std()
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect();
        assert_eq!(command.as_std().get_program(), "parec");
        assert_eq!(
            args,
            [
                "--device=@DEFAULT_MONITOR@",
                "--raw",
                "--format=s16le",
                "--rate=48000",
                "--channels=2",
                "--latency-msec=10"
            ]
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[ignore = "requires PulseAudio and a tone playing on the default sink"]
    async fn manual_pulse_opus() {
        assert!(available());
        let (capture, mut pcm) = capture().unwrap();
        let pid = capture.child.id().unwrap();
        let mut encoder = encoder().unwrap();
        let mut output = [0; 4000];
        let mut frames = 0;
        let mut energy = 0_u64;
        while frames < 50 {
            let frame = tokio::time::timeout(Duration::from_secs(5), pcm.recv())
                .await
                .unwrap()
                .unwrap();
            energy += frame
                .iter()
                .map(|sample| i64::from(*sample).pow(2) as u64)
                .sum::<u64>();
            assert!(encoder.encode(&frame, &mut output).unwrap() > 0);
            frames += 1;
        }
        drop(capture);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
        assert!(energy > 0, "play a tone on the default sink");
        println!(
            "PulseAudio capture: {frames} Opus frames (20ms stereo 48kHz), PCM energy={energy}; parec child stopped"
        );
    }
}
