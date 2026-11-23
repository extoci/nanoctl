use std::sync::Arc;
use std::time::Duration;

#[cfg(any(target_os = "linux", target_os = "windows"))]
use anyhow::Context;
use anyhow::Result;
use tokio::sync::{mpsc, watch};
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;

const SAMPLES: usize = 960 * 2;
const FRAME_DURATION: Duration = Duration::from_millis(20);

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
    #[cfg(target_os = "windows")]
    {
        windows_worker().is_some_and(|worker| {
            let (ready, result) = std::sync::mpsc::sync_channel(1);
            worker.commands.send(WindowsCommand::Probe(ready)).is_ok()
                && result.recv().unwrap_or(false)
        })
    }
    #[cfg(target_os = "macos")]
    {
        false
    }
}

fn encoder() -> Result<opus::Encoder> {
    let mut encoder =
        opus::Encoder::new(48_000, opus::Channels::Stereo, opus::Application::LowDelay)?;
    encoder.set_bitrate(opus::Bitrate::Bits(128_000))?;
    Ok(encoder)
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
    Probe(std::sync::mpsc::SyncSender<bool>),
    Start {
        pcm: mpsc::Sender<Vec<i16>>,
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
    use cpal::traits::HostTrait;
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
                    if unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_err() {
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
                    while let Ok(command) = requests.recv() {
                        match command {
                            WindowsCommand::Probe(ready) => {
                                let _ = ready
                                    .send(cpal::default_host().default_output_device().is_some());
                            }
                            WindowsCommand::Start { pcm, ready } => {
                                let result = windows_capture(pcm).map(|stream| {
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
fn capture() -> Result<(Capture, mpsc::Receiver<Vec<i16>>)> {
    let worker = windows_worker().context("cannot initialize audio worker")?;
    let (pcm, receiver) = mpsc::channel(3);
    let (ready, started) = std::sync::mpsc::sync_channel(1);
    worker.commands.send(WindowsCommand::Start { pcm, ready })?;
    let id = started.recv()?.map_err(anyhow::Error::msg)?;
    Ok((
        Capture {
            commands: worker.commands.clone(),
            id,
        },
        receiver,
    ))
}

#[cfg(target_os = "windows")]
fn windows_capture(sender: mpsc::Sender<Vec<i16>>) -> Result<cpal::Stream> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    let device = cpal::default_host()
        .default_output_device()
        .context("no output device")?;
    let config = device.default_output_config()?;
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => windows_stream::<f32>(&device, &config.into(), sender),
        cpal::SampleFormat::I16 => windows_stream::<i16>(&device, &config.into(), sender),
        cpal::SampleFormat::U16 => windows_stream::<u16>(&device, &config.into(), sender),
        format => anyhow::bail!("unsupported loopback format {format:?}"),
    }?;
    stream.play()?;
    Ok(stream)
}

#[cfg(target_os = "windows")]
fn windows_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    sender: mpsc::Sender<Vec<i16>>,
) -> Result<cpal::Stream>
where
    T: cpal::SizedSample + cpal::Sample,
    f32: cpal::FromSample<T>,
{
    use cpal::traits::DeviceTrait;
    let channels = usize::from(config.channels);
    let rate = u64::from(config.sample_rate.0);
    let mut phase = 0_u64;
    let mut frame = Vec::with_capacity(SAMPLES);
    Ok(device.build_input_stream(
        config,
        move |data: &[T], _| {
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
        |error| tracing::warn!(%error, "loopback capture failed"),
        None,
    )?)
}

#[cfg(target_os = "macos")]
fn capture() -> Result<((), mpsc::Receiver<Vec<i16>>)> {
    anyhow::bail!("system audio is unsupported on macOS")
}

pub async fn run(
    track: Arc<TrackLocalStaticSample>,
    peer: Arc<RTCPeerConnection>,
    mut shutdown: watch::Receiver<bool>,
) {
    let result = run_inner(track, peer, &mut shutdown).await;
    if let Err(error) = result {
        tracing::warn!(%error, "system audio stopped");
    }
}

async fn run_inner(
    track: Arc<TrackLocalStaticSample>,
    peer: Arc<RTCPeerConnection>,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<()> {
    if *shutdown.borrow() {
        return Ok(());
    }
    while peer.connection_state() != RTCPeerConnectionState::Connected {
        tokio::select! {
            _ = shutdown.changed() => return Ok(()),
            _ = tokio::time::sleep(FRAME_DURATION) => {}
        }
    }
    let (_capture, mut pcm) = capture()?;
    let mut encoder = encoder()?;
    let (encoded, mut packets) = mpsc::channel(2);
    tokio::task::spawn_blocking(move || {
        let mut output = [0; 4000];
        while let Some(frame) = pcm.blocking_recv() {
            if encoded.is_closed() {
                break;
            }
            if frame.len() != SAMPLES {
                continue;
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
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            packet = packets.recv() => {
                let Some(packet) = packet else { break; };
                let sample = webrtc::media::Sample { data: packet.into(), duration: FRAME_DURATION, ..Default::default() };
                let _ = tokio::time::timeout(FRAME_DURATION * 2, track.write_sample(&sample)).await;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_worker_probes_from_multiple_calling_threads() {
        let worker = windows_worker().expect("audio worker initializes COM");
        let expected = available();
        let probes: Vec<_> = (0..16).map(|_| std::thread::spawn(available)).collect();
        for probe in probes {
            assert_eq!(probe.join().unwrap(), expected);
        }
        worker
            .commands
            .send(WindowsCommand::Stop(u64::MAX))
            .unwrap();
        if !expected {
            assert!(capture().is_err());
        }
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
