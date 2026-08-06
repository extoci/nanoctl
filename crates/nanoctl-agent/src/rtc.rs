use std::sync::Arc;
#[cfg(feature = "media")]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(feature = "media")]
use std::time::Duration;
use std::time::SystemTime;

use anyhow::{Context, Result};
use interceptor::registry::Registry;
#[cfg(feature = "media")]
use rtcp::payload_feedbacks::full_intra_request::FullIntraRequest;
#[cfg(feature = "media")]
use rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication;
#[cfg(feature = "media")]
use rtcp::payload_feedbacks::receiver_estimated_maximum_bitrate::ReceiverEstimatedMaximumBitrate;
use serde::{Deserialize, Serialize};
use webrtc::api::APIBuilder;
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::{MIME_TYPE_H264, MediaEngine};
#[cfg(feature = "media")]
use webrtc::data_channel::RTCDataChannel;
#[cfg(feature = "media")]
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::ice_transport::ice_candidate::{RTCIceCandidate, RTCIceCandidateInit};
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::policy::ice_transport_policy::RTCIceTransportPolicy;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::rtp_transceiver::RTCPFeedback;
use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use zeroize::Zeroizing;

use crate::control_plane::ControlPlane;

const PROTOCOL_VERSION: u8 = 1;
// Level 5.2 is the highest H.264 level supported by the configured 4K60 release envelope. The
// WebRTC matcher treats the level byte asymmetrically, so this remains compatible with Chromium's
// lower-level offer while no longer advertising a 720p30 ceiling for a 4K-capable sender.
const H264_PROFILE_LEVEL_ID: &str = "42e034";
#[cfg(any(feature = "media", test))]
pub(crate) const MAX_CONTROL_MESSAGE_BYTES: usize = 64 * 1024;
#[cfg(feature = "media")]
const RELIABLE_CONTROL_QUEUE_CAPACITY: usize = 64;

#[cfg(any(feature = "media", test))]
#[derive(Debug, PartialEq, Eq)]
enum InputAdmission {
    Accepted,
    Full,
    Oversized,
    Closed,
}

#[cfg(any(feature = "media", test))]
fn admit_input_message(
    sender: &tokio::sync::mpsc::Sender<Vec<u8>>,
    bytes: Vec<u8>,
) -> InputAdmission {
    if bytes.len() > MAX_CONTROL_MESSAGE_BYTES {
        return InputAdmission::Oversized;
    }
    match sender.try_send(bytes) {
        Ok(()) => InputAdmission::Accepted,
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => InputAdmission::Full,
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => InputAdmission::Closed,
    }
}

pub struct HostPeer {
    peer: Arc<RTCPeerConnection>,
    publisher: SignalPublisher,
    session_id: String,
    sequence: Arc<AtomicU64>,
    publication_lock: Arc<tokio::sync::Mutex<()>>,
    #[cfg(feature = "media")]
    keyframe_requests: tokio::sync::watch::Sender<u64>,
    #[cfg(feature = "media")]
    bitrate_estimate_kbps: tokio::sync::watch::Sender<u32>,
    #[cfg(feature = "media")]
    display_selection: tokio::sync::watch::Sender<String>,
    #[cfg_attr(not(feature = "media"), allow(dead_code))]
    video: Arc<TrackLocalStaticSample>,
}

#[derive(Clone)]
struct SignalPublisher {
    inner: SignalPublisherInner,
}

#[derive(Clone)]
enum SignalPublisherInner {
    ControlPlane {
        client: ControlPlane,
        token: Arc<Zeroizing<String>>,
    },
    #[cfg(test)]
    Test(tokio::sync::mpsc::UnboundedSender<TestSignal>),
}

#[cfg(test)]
#[derive(Debug)]
struct TestSignal {
    session_id: String,
    sequence: u64,
    envelope: String,
}

impl SignalPublisher {
    fn control_plane(client: ControlPlane, token: Arc<Zeroizing<String>>) -> Self {
        Self {
            inner: SignalPublisherInner::ControlPlane { client, token },
        }
    }

    #[cfg(test)]
    fn test(sender: tokio::sync::mpsc::UnboundedSender<TestSignal>) -> Self {
        Self {
            inner: SignalPublisherInner::Test(sender),
        }
    }

    async fn send(&self, session_id: &str, sequence: u64, envelope: &str) -> Result<()> {
        match &self.inner {
            SignalPublisherInner::ControlPlane { client, token } => {
                client
                    .send_signal(token, session_id, sequence, envelope)
                    .await
            }
            #[cfg(test)]
            SignalPublisherInner::Test(sender) => sender
                .send(TestSignal {
                    session_id: session_id.to_owned(),
                    sequence,
                    envelope: envelope.to_owned(),
                })
                .map_err(|_| anyhow::anyhow!("test signal receiver closed")),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SignalEnvelope {
    version: u8,
    session_id: String,
    sequence: u64,
    sender: String,
    sent_at: u64,
    payload: SignalPayload,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum SignalPayload {
    Offer {
        sdp: String,
    },
    IceCandidate {
        candidate: String,
        #[serde(rename = "sdpMid")]
        sdp_mid: Option<String>,
        #[serde(rename = "sdpMLineIndex")]
        sdp_mline_index: Option<u16>,
        #[serde(rename = "usernameFragment", default)]
        username_fragment: Option<String>,
    },
    IceComplete,
    End {
        reason: String,
    },
    #[serde(other)]
    Unsupported,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OutgoingEnvelope<'a> {
    version: u8,
    session_id: &'a str,
    sequence: u64,
    sender: &'static str,
    sent_at: u64,
    payload: OutgoingPayload,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum OutgoingPayload {
    Answer {
        sdp: String,
    },
    IceCandidate {
        candidate: String,
        #[serde(rename = "sdpMid")]
        sdp_mid: Option<String>,
        #[serde(rename = "sdpMLineIndex")]
        sdp_mline_index: Option<u16>,
        #[serde(rename = "usernameFragment")]
        username_fragment: Option<String>,
    },
    IceComplete,
    End {
        reason: String,
    },
}

impl HostPeer {
    pub async fn answer(
        control_plane: ControlPlane,
        token: Arc<Zeroizing<String>>,
        session_id: String,
        serialized_offer: &str,
        ice_servers: Vec<RTCIceServer>,
        ice_transport_policy: RTCIceTransportPolicy,
        allow_remote_input: bool,
    ) -> Result<Self> {
        Self::answer_with_publisher(
            SignalPublisher::control_plane(control_plane, token),
            session_id,
            serialized_offer,
            ice_servers,
            ice_transport_policy,
            allow_remote_input,
        )
        .await
    }

    async fn answer_with_publisher(
        publisher: SignalPublisher,
        session_id: String,
        serialized_offer: &str,
        ice_servers: Vec<RTCIceServer>,
        ice_transport_policy: RTCIceTransportPolicy,
        allow_remote_input: bool,
    ) -> Result<Self> {
        let offer = parse_offer(serialized_offer, &session_id)?;
        let mut media_engine = MediaEngine::default();
        media_engine.register_default_codecs()?;
        let registry = register_default_interceptors(Registry::new(), &mut media_engine)?;
        let api = APIBuilder::new()
            .with_media_engine(media_engine)
            .with_interceptor_registry(registry)
            .build();
        #[cfg(not(feature = "media"))]
        let _ = allow_remote_input;
        let peer = Arc::new(
            api.new_peer_connection(RTCConfiguration {
                ice_servers,
                ice_transport_policy,
                ..Default::default()
            })
            .await?,
        );
        #[cfg(feature = "media")]
        let (display_selection, _) = tokio::sync::watch::channel(String::new());
        #[cfg(feature = "media")]
        {
            let input = if allow_remote_input {
                match crate::input::InputController::new() {
                    Ok(input) => Some(Arc::new(std::sync::Mutex::new(input))),
                    Err(error) => {
                        tracing::warn!(error = %error, "remote input is unavailable");
                        None
                    }
                }
            } else {
                None
            };
            if let Some(input) = &input {
                let input = Arc::downgrade(input);
                tokio::spawn(async move {
                    let mut interval = tokio::time::interval(Duration::from_millis(500));
                    loop {
                        interval.tick().await;
                        let Some(input) = input.upgrade() else {
                            break;
                        };
                        if let Ok(mut input) = input.lock() {
                            input.release_if_idle(Duration::from_secs(2));
                        }
                    }
                });
            }
            let (control_sender, control_receiver) =
                tokio::sync::mpsc::channel(RELIABLE_CONTROL_QUEUE_CAPACITY);
            let (pointer_sender, pointer_receiver) = tokio::sync::mpsc::channel(1);
            let fail_safe_release_pending = Arc::new(AtomicBool::new(false));
            if let Some(input) = &input {
                spawn_input_worker(
                    control_receiver,
                    input.clone(),
                    crate::input::InputLane::Reliable,
                    Some(display_selection.clone()),
                );
                spawn_input_worker(
                    pointer_receiver,
                    input.clone(),
                    crate::input::InputLane::PointerMotion,
                    None,
                );
            }
            peer.on_data_channel(Box::new(move |channel: Arc<RTCDataChannel>| {
                let input = input.clone();
                if input.is_none() {
                    return Box::pin(async {});
                }
                let (sender, drop_when_full) = match channel.label() {
                    "nanoctl.control.v1" => (control_sender.clone(), false),
                    "nanoctl.pointer.v1" => (pointer_sender.clone(), true),
                    _ => return Box::pin(async {}),
                };
                let message_input = input.clone();
                let release_pending = fail_safe_release_pending.clone();
                channel.on_message(Box::new(move |message: DataChannelMessage| {
                    let sender = sender.clone();
                    let input = message_input.clone();
                    let release_pending = release_pending.clone();
                    Box::pin(async move {
                        if !message.is_string {
                            return;
                        }
                        let admission = admit_input_message(&sender, message.data.to_vec());
                        let must_release = !drop_when_full
                            && matches!(
                                admission,
                                InputAdmission::Full | InputAdmission::Oversized
                            );
                        if must_release
                            && release_pending
                                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                                .is_ok()
                        {
                            tracing::warn!(
                                ?admission,
                                "reliable input flood triggered fail-safe release"
                            );
                            let release_pending = release_pending.clone();
                            tokio::task::spawn_blocking(move || {
                                if let Some(input) = input
                                    && let Ok(mut input) = input.lock()
                                {
                                    input.release_all();
                                }
                                release_pending.store(false, Ordering::Release);
                            });
                        }
                    })
                }));
                channel.on_close(Box::new(move || {
                    let input = input.clone();
                    Box::pin(async move {
                        if let Some(input) = input
                            && let Ok(mut input) = input.lock()
                        {
                            input.release_all();
                        }
                    })
                }));
                Box::pin(async {})
            }));
        }
        let video = Arc::new(TrackLocalStaticSample::new(
            RTCRtpCodecCapability {
                mime_type: MIME_TYPE_H264.to_owned(),
                clock_rate: 90_000,
                channels: 0,
                sdp_fmtp_line: format!(
                    "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id={H264_PROFILE_LEVEL_ID}"
                ),
                rtcp_feedback: vec![
                    RTCPFeedback {
                        typ: "nack".to_owned(),
                        parameter: "".to_owned(),
                    },
                    RTCPFeedback {
                        typ: "nack".to_owned(),
                        parameter: "pli".to_owned(),
                    },
                    RTCPFeedback {
                        typ: "ccm".to_owned(),
                        parameter: "fir".to_owned(),
                    },
                    RTCPFeedback {
                        typ: "goog-remb".to_owned(),
                        parameter: "".to_owned(),
                    },
                ],
            },
            "desktop".to_owned(),
            "nanoctl".to_owned(),
        ));
        let sender = peer.add_track(video.clone()).await?;
        #[cfg(feature = "media")]
        let (keyframe_requests, _) = tokio::sync::watch::channel(0_u64);
        #[cfg(feature = "media")]
        let keyframe_feedback = keyframe_requests.clone();
        #[cfg(feature = "media")]
        let (bitrate_estimate_kbps, _) = tokio::sync::watch::channel(0_u32);
        #[cfg(feature = "media")]
        let bitrate_feedback = bitrate_estimate_kbps.clone();
        tokio::spawn(async move {
            while let Ok((_packets, _attributes)) = sender.read_rtcp().await {
                #[cfg(feature = "media")]
                {
                    if _packets.iter().any(|packet| {
                        packet.as_any().is::<PictureLossIndication>()
                            || packet.as_any().is::<FullIntraRequest>()
                    }) {
                        keyframe_feedback.send_modify(|generation| {
                            *generation = generation.wrapping_add(1);
                        });
                    }
                    if let Some(estimate) = _packets.iter().find_map(|packet| {
                        packet
                            .as_any()
                            .downcast_ref::<ReceiverEstimatedMaximumBitrate>()
                    }) && estimate.bitrate.is_finite()
                        && estimate.bitrate > 0.0
                    {
                        let kbps = (estimate.bitrate / 1_000.0).min(u32::MAX as f32) as u32;
                        bitrate_feedback.send_replace(kbps.max(1));
                    }
                }
            }
        });

        let sequence = Arc::new(AtomicU64::new(1));
        // ICE callbacks are asynchronous and can run as soon as local description is installed.
        // Serialize them with the answer publication so a browser never observes a host
        // candidate before the SDP that gives that candidate a remote description.
        let publication_lock = Arc::new(tokio::sync::Mutex::new(()));
        peer.on_ice_candidate({
            let publisher = publisher.clone();
            let session_id = session_id.clone();
            let sequence = sequence.clone();
            let publication_lock = publication_lock.clone();
            Box::new(move |candidate: Option<RTCIceCandidate>| {
                let publisher = publisher.clone();
                let session_id = session_id.clone();
                let sequence = sequence.clone();
                let publication_lock = publication_lock.clone();
                Box::pin(async move {
                    let _publication_guard = publication_lock.lock().await;
                    let payload = match candidate {
                        Some(candidate) => match candidate.to_json() {
                            Ok(candidate) => OutgoingPayload::IceCandidate {
                                candidate: candidate.candidate,
                                sdp_mid: candidate.sdp_mid,
                                sdp_mline_index: candidate.sdp_mline_index,
                                username_fragment: candidate.username_fragment,
                            },
                            Err(_) => return,
                        },
                        None => OutgoingPayload::IceComplete,
                    };
                    let index = sequence.fetch_add(1, Ordering::Relaxed);
                    if let Ok(envelope) = serialize(&session_id, index, payload) {
                        let _ = publisher.send(&session_id, index, &envelope).await;
                    }
                })
            })
        });

        let _publication_guard = publication_lock.lock().await;
        peer.set_remote_description(RTCSessionDescription::offer(offer)?)
            .await?;
        let answer = peer.create_answer(None).await?;
        peer.set_local_description(answer).await?;
        let local = peer
            .local_description()
            .await
            .context("WebRTC answer was not created")?;
        // Sequence zero is reserved for the answer. Candidate callbacks begin at one.
        let envelope = serialize(&session_id, 0, OutgoingPayload::Answer { sdp: local.sdp })?;
        publisher.send(&session_id, 0, &envelope).await?;
        drop(_publication_guard);
        Ok(Self {
            peer,
            publisher,
            session_id,
            sequence,
            publication_lock,
            #[cfg(feature = "media")]
            keyframe_requests,
            #[cfg(feature = "media")]
            bitrate_estimate_kbps,
            #[cfg(feature = "media")]
            display_selection,
            video,
        })
    }

    pub async fn add_signal(
        &self,
        serialized: &str,
        expected_session: &str,
        expected_sequence: u64,
    ) -> Result<bool> {
        if serialized.len() > 1_100_000 {
            anyhow::bail!("signal exceeds maximum size");
        }
        let envelope: SignalEnvelope = serde_json::from_str(serialized)?;
        if envelope.version != PROTOCOL_VERSION
            || envelope.session_id != expected_session
            || envelope.sequence != expected_sequence
            || envelope.sender != "controller"
            || envelope.sent_at == 0
        {
            anyhow::bail!("signal identity does not match the session");
        }
        match envelope.payload {
            SignalPayload::IceCandidate {
                candidate,
                sdp_mid,
                sdp_mline_index,
                username_fragment,
            } => {
                if candidate.is_empty()
                    || candidate.len() > 8_192
                    || sdp_mid.as_ref().is_some_and(|value| value.len() > 256)
                    || username_fragment
                        .as_ref()
                        .is_some_and(|value| value.is_empty() || value.len() > 256)
                {
                    anyhow::bail!("ICE candidate is invalid");
                }
                self.peer
                    .add_ice_candidate(RTCIceCandidateInit {
                        candidate,
                        sdp_mid,
                        sdp_mline_index,
                        username_fragment,
                    })
                    .await?;
                Ok(true)
            }
            SignalPayload::IceComplete => Ok(true),
            SignalPayload::End { reason } => {
                if reason.is_empty() || reason.len() > 512 {
                    anyhow::bail!("end reason is invalid");
                }
                tracing::info!(reason_bytes = reason.len(), "controller ended session");
                self.peer.close().await?;
                Ok(false)
            }
            SignalPayload::Offer { sdp } => {
                if sdp.is_empty() || sdp.len() > 1_000_000 {
                    anyhow::bail!("restart offer SDP is invalid");
                }
                let _publication_guard = self.publication_lock.lock().await;
                self.peer
                    .set_remote_description(RTCSessionDescription::offer(sdp)?)
                    .await?;
                let answer = self.peer.create_answer(None).await?;
                self.peer.set_local_description(answer).await?;
                let local = self
                    .peer
                    .local_description()
                    .await
                    .context("WebRTC restart answer was not created")?;
                let index = self.sequence.fetch_add(1, Ordering::Relaxed);
                let response = serialize(
                    &self.session_id,
                    index,
                    OutgoingPayload::Answer { sdp: local.sdp },
                )?;
                self.publisher
                    .send(&self.session_id, index, &response)
                    .await?;
                Ok(true)
            }
            SignalPayload::Unsupported => Ok(true),
        }
    }

    pub async fn close(&self) -> Result<()> {
        self.peer.close().await?;
        Ok(())
    }

    pub async fn fail(&self, reason: &str) -> Result<()> {
        let reason = reason.chars().take(256).collect::<String>();
        let index = self.sequence.fetch_add(1, Ordering::Relaxed);
        let envelope = serialize(&self.session_id, index, OutgoingPayload::End { reason })?;
        let signal_result = self
            .publisher
            .send(&self.session_id, index, &envelope)
            .await;
        let close_result = self.peer.close().await;
        signal_result?;
        close_result?;
        Ok(())
    }

    pub fn connection_failed(&self) -> bool {
        self.peer.connection_state() == RTCPeerConnectionState::Failed
    }

    #[cfg(feature = "media")]
    pub fn video_track(&self) -> Arc<TrackLocalStaticSample> {
        self.video.clone()
    }

    #[cfg(feature = "media")]
    pub fn keyframe_requests(&self) -> tokio::sync::watch::Receiver<u64> {
        self.keyframe_requests.subscribe()
    }

    #[cfg(feature = "media")]
    pub fn bitrate_estimate_kbps(&self) -> tokio::sync::watch::Receiver<u32> {
        self.bitrate_estimate_kbps.subscribe()
    }

    #[cfg(feature = "media")]
    pub fn display_selection(&self) -> tokio::sync::watch::Receiver<String> {
        self.display_selection.subscribe()
    }
}

#[cfg(feature = "media")]
fn spawn_input_worker(
    mut receiver: tokio::sync::mpsc::Receiver<Vec<u8>>,
    input: Arc<std::sync::Mutex<crate::input::InputController>>,
    lane: crate::input::InputLane,
    display_selection: Option<tokio::sync::watch::Sender<String>>,
) {
    tokio::spawn(async move {
        while let Some(bytes) = receiver.recv().await {
            let input = input.clone();
            let result = tokio::task::spawn_blocking(move || {
                input
                    .lock()
                    .map_err(|_| anyhow::anyhow!("input controller lock poisoned"))?
                    .dispatch(&bytes, lane)
            })
            .await;
            match result {
                Ok(Ok(Some(display_id))) => {
                    if let Some(selection) = &display_selection {
                        selection.send_replace(display_id);
                    }
                }
                Ok(Ok(None)) => {}
                Ok(Err(error)) => tracing::warn!(error = %error, "control message rejected"),
                Err(error) => {
                    tracing::warn!(error = %error, "input worker stopped unexpectedly");
                    break;
                }
            }
        }
    });
}

fn parse_offer(serialized: &str, expected_session: &str) -> Result<String> {
    if serialized.len() > 1_100_000 {
        anyhow::bail!("signal exceeds maximum size");
    }
    let envelope: SignalEnvelope =
        serde_json::from_str(serialized).context("offer envelope is invalid")?;
    if envelope.version != PROTOCOL_VERSION
        || envelope.session_id != expected_session
        || envelope.sequence != 0
        || envelope.sender != "controller"
        || envelope.sent_at == 0
    {
        anyhow::bail!("offer identity does not match the session");
    }
    match envelope.payload {
        SignalPayload::Offer { sdp } if !sdp.is_empty() && sdp.len() <= 1_000_000 => Ok(sdp),
        _ => anyhow::bail!("signal is not a valid offer"),
    }
}

pub fn is_offer_for_session(serialized: &str, expected_session: &str) -> bool {
    parse_offer(serialized, expected_session).is_ok()
}

fn serialize(session_id: &str, sequence: u64, payload: OutgoingPayload) -> Result<String> {
    Ok(serde_json::to_string(&OutgoingEnvelope {
        version: PROTOCOL_VERSION,
        session_id,
        sequence,
        sender: "host",
        sent_at: SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_millis() as u64,
        payload,
    })?)
}

pub async fn report_negotiation_failure(
    control_plane: &ControlPlane,
    token: &Zeroizing<String>,
    session_id: &str,
    reason: &str,
) -> Result<()> {
    // Sequence zero is otherwise reserved for the initial answer. If peer construction fails there
    // is no answer, so it is also the only host sequence that can have been published.
    let reason = reason.chars().take(256).collect::<String>();
    let envelope = serialize(session_id, 0, OutgoingPayload::End { reason })?;
    control_plane
        .send_signal(token, session_id, 0, &envelope)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use serde_json::Value;
    use tokio::time::{Duration, Instant, timeout};
    use webrtc::peer_connection::offer_answer_options::RTCOfferOptions;
    use webrtc::rtp_transceiver::rtp_codec::RTPCodecType;

    #[test]
    fn rejects_cross_session_offer() {
        let value = r#"{"version":1,"sessionId":"other","sequence":0,"sender":"controller","sentAt":1,"payload":{"type":"offer","sdp":"v=0"}}"#;
        assert!(parse_offer(value, "expected").is_err());
    }

    #[test]
    fn rejects_noninitial_or_missing_timestamp_offer() {
        let noninitial = r#"{"version":1,"sessionId":"session","sequence":7,"sender":"controller","sentAt":1,"payload":{"type":"offer","sdp":"v=0"}}"#;
        let missing_timestamp = r#"{"version":1,"sessionId":"session","sequence":0,"sender":"controller","sentAt":0,"payload":{"type":"offer","sdp":"v=0"}}"#;
        assert!(parse_offer(noninitial, "session").is_err());
        assert!(parse_offer(missing_timestamp, "session").is_err());
    }

    #[test]
    fn does_not_mistake_candidate_text_for_an_offer() {
        let value = r#"{"version":1,"sessionId":"session","sequence":0,"sender":"controller","sentAt":1,"payload":{"type":"ice-candidate","candidate":"candidate with \\\"type\\\":\\\"offer\\\" text","sdpMid":"0","sdpMLineIndex":0}}"#;
        assert!(!is_offer_for_session(value, "session"));
    }

    #[test]
    fn serializes_host_identity() {
        let value = serialize("session", 3, OutgoingPayload::IceComplete).unwrap();
        assert!(value.contains(r#""sender":"host""#));
        assert!(value.contains(r#""sequence":3"#));
    }

    #[test]
    fn serializes_bounded_terminal_negotiation_reason() {
        let value = serialize(
            "session",
            0,
            OutgoingPayload::End {
                reason: "agent could not initialize the remote desktop session".to_owned(),
            },
        )
        .unwrap();
        let envelope: Value = serde_json::from_str(&value).unwrap();
        assert_eq!(envelope["sequence"], 0);
        assert_eq!(envelope["payload"]["type"], "end");
        assert_eq!(
            envelope["payload"]["reason"],
            "agent could not initialize the remote desktop session"
        );
    }

    #[test]
    fn serializes_terminal_media_status() {
        let value = serialize(
            "session",
            4,
            OutgoingPayload::End {
                reason: "media pipeline failed".to_owned(),
            },
        )
        .unwrap();
        assert!(value.contains(r#""type":"end""#));
        assert!(value.contains(r#""reason":"media pipeline failed""#));
    }

    #[test]
    fn bounds_control_queue_memory_and_work() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        assert_eq!(
            admit_input_message(&sender, vec![0; MAX_CONTROL_MESSAGE_BYTES]),
            InputAdmission::Accepted
        );
        assert_eq!(
            admit_input_message(&sender, vec![0; 1]),
            InputAdmission::Full
        );
        assert_eq!(
            receiver.try_recv().unwrap().len(),
            MAX_CONTROL_MESSAGE_BYTES
        );
        assert_eq!(
            admit_input_message(&sender, vec![0; MAX_CONTROL_MESSAGE_BYTES + 1]),
            InputAdmission::Oversized
        );
        drop(receiver);
        assert_eq!(
            admit_input_message(&sender, vec![0; 1]),
            InputAdmission::Closed
        );
    }

    proptest! {
        #[test]
        fn arbitrary_signaling_text_never_bypasses_offer_identity(
            version in any::<u8>(),
            sequence in any::<u64>(),
            sender in ".{0,32}",
            sent_at in any::<u64>(),
            session_id in ".{0,64}",
            expected_session in ".{0,64}",
            sdp in ".{0,2048}",
        ) {
            let envelope = serde_json::json!({
                "version": version,
                "sessionId": session_id,
                "sequence": sequence,
                "sender": sender,
                "sentAt": sent_at,
                "payload": {"type": "offer", "sdp": sdp},
            })
            .to_string();
            let accepted = parse_offer(&envelope, &expected_session).is_ok();
            let should_accept = version == PROTOCOL_VERSION
                && session_id == expected_session
                && sequence == 0
                && sender == "controller"
                && sent_at != 0
                && !sdp.is_empty();
            prop_assert_eq!(accepted, should_accept);
        }

        #[test]
        fn arbitrary_text_does_not_panic_the_signaling_parser(
            serialized in proptest::collection::vec(any::<u8>(), 0..4096),
            expected_session in ".{0,64}",
        ) {
            if let Ok(text) = std::str::from_utf8(&serialized) {
                let _ = is_offer_for_session(text, &expected_session);
            }
        }
    }

    #[tokio::test]
    async fn production_host_rejects_malformed_sdp() {
        let malformed_offer = serde_json::json!({
            "version": PROTOCOL_VERSION,
            "sessionId": "malformed-session",
            "sequence": 0,
            "sender": "controller",
            "sentAt": 1,
            "payload": {"type": "offer", "sdp": "not-an-sdp"},
        })
        .to_string();
        let (signals_tx, _signals_rx) = tokio::sync::mpsc::unbounded_channel();

        let result = HostPeer::answer_with_publisher(
            SignalPublisher::test(signals_tx),
            "malformed-session".to_owned(),
            &malformed_offer,
            Vec::new(),
            RTCIceTransportPolicy::All,
            false,
        )
        .await;

        assert!(result.is_err(), "malformed SDP must not create a host peer");
    }

    #[tokio::test]
    async fn production_host_peer_connects_to_a_real_controller_peer() {
        let mut media_engine = MediaEngine::default();
        media_engine.register_default_codecs().unwrap();
        let registry = register_default_interceptors(Registry::new(), &mut media_engine).unwrap();
        let controller = Arc::new(
            APIBuilder::new()
                .with_media_engine(media_engine)
                .with_interceptor_registry(registry)
                .build()
                .new_peer_connection(RTCConfiguration::default())
                .await
                .unwrap(),
        );
        controller
            .add_transceiver_from_kind(RTPCodecType::Video, None)
            .await
            .unwrap();
        let control = controller
            .create_data_channel("nanoctl.control.v1", None)
            .await
            .unwrap();
        let opened = Arc::new(tokio::sync::Notify::new());
        control.on_open({
            let opened = opened.clone();
            Box::new(move || {
                let opened = opened.clone();
                Box::pin(async move {
                    opened.notify_one();
                })
            })
        });

        let offer = controller.create_offer(None).await.unwrap();
        controller.set_local_description(offer).await.unwrap();
        let mut gathering_complete = controller.gathering_complete_promise().await;
        let _ = gathering_complete.recv().await;
        let offer = controller.local_description().await.unwrap();
        let session_id = "integration-session";
        let serialized_offer = serde_json::json!({
            "version": PROTOCOL_VERSION,
            "sessionId": session_id,
            "sequence": 0,
            "sender": "controller",
            "sentAt": 1,
            "payload": {"type": "offer", "sdp": offer.sdp},
        })
        .to_string();

        let (signals_tx, mut signals_rx) = tokio::sync::mpsc::unbounded_channel();
        let host = HostPeer::answer_with_publisher(
            SignalPublisher::test(signals_tx),
            session_id.to_owned(),
            &serialized_offer,
            Vec::new(),
            RTCIceTransportPolicy::All,
            false,
        )
        .await
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut answer_applied = false;
        let mut first_signal = true;
        while Instant::now() < deadline
            && (controller.connection_state() != RTCPeerConnectionState::Connected
                || !answer_applied)
        {
            let Some(signal) = timeout(Duration::from_millis(250), signals_rx.recv())
                .await
                .ok()
                .flatten()
            else {
                continue;
            };
            assert_eq!(signal.session_id, session_id);
            let envelope: Value = serde_json::from_str(&signal.envelope).unwrap();
            assert_eq!(envelope["sequence"].as_u64(), Some(signal.sequence));
            if first_signal {
                assert_eq!(envelope["payload"]["type"].as_str(), Some("answer"));
                first_signal = false;
            }
            match envelope["payload"]["type"].as_str() {
                Some("answer") => {
                    assert_eq!(signal.sequence, 0);
                    let sdp = envelope["payload"]["sdp"].as_str().unwrap().to_owned();
                    controller
                        .set_remote_description(RTCSessionDescription::answer(sdp).unwrap())
                        .await
                        .unwrap();
                    answer_applied = true;
                }
                Some("ice-candidate") => {
                    controller
                        .add_ice_candidate(RTCIceCandidateInit {
                            candidate: envelope["payload"]["candidate"]
                                .as_str()
                                .unwrap()
                                .to_owned(),
                            sdp_mid: envelope["payload"]["sdpMid"].as_str().map(str::to_owned),
                            sdp_mline_index: envelope["payload"]["sdpMLineIndex"]
                                .as_u64()
                                .map(|value| value as u16),
                            username_fragment: None,
                        })
                        .await
                        .unwrap();
                }
                Some("ice-complete") => {}
                other => panic!("unexpected host signal: {other:?}"),
            }
        }

        assert!(answer_applied, "host did not publish its SDP answer");
        assert_eq!(
            controller.connection_state(),
            RTCPeerConnectionState::Connected
        );
        timeout(Duration::from_secs(3), opened.notified())
            .await
            .expect("negotiated control channel did not open");

        let malformed_candidate = serde_json::json!({
            "version": PROTOCOL_VERSION,
            "sessionId": session_id,
            "sequence": 1,
            "sender": "controller",
            "sentAt": 2,
            "payload": {
                "type": "ice-candidate",
                "candidate": "",
                "sdpMid": "0",
                "sdpMLineIndex": 0,
            },
        })
        .to_string();
        assert!(
            host.add_signal(&malformed_candidate, session_id, 1)
                .await
                .is_err(),
            "malformed ICE candidate must be rejected"
        );

        while signals_rx.try_recv().is_ok() {}
        let initial_remote_sdp = controller.remote_description().await.unwrap().sdp;
        let restart_offer = controller
            .create_offer(Some(RTCOfferOptions {
                ice_restart: true,
                ..Default::default()
            }))
            .await
            .unwrap();
        controller
            .set_local_description(restart_offer)
            .await
            .unwrap();
        let mut restart_gathering_complete = controller.gathering_complete_promise().await;
        let _ = restart_gathering_complete.recv().await;
        let restart_offer = controller.local_description().await.unwrap();
        let restart_envelope = serde_json::json!({
            "version": PROTOCOL_VERSION,
            "sessionId": session_id,
            "sequence": 2,
            "sender": "controller",
            "sentAt": 3,
            "payload": {"type": "offer", "sdp": restart_offer.sdp},
        })
        .to_string();
        assert!(
            host.add_signal(&restart_envelope, session_id, 2)
                .await
                .unwrap()
        );

        let restart_deadline = Instant::now() + Duration::from_secs(10);
        let mut restart_answer_applied = false;
        let mut pending_candidates = Vec::new();
        while Instant::now() < restart_deadline && !restart_answer_applied {
            let Some(signal) = timeout(Duration::from_millis(250), signals_rx.recv())
                .await
                .ok()
                .flatten()
            else {
                continue;
            };
            let envelope: Value = serde_json::from_str(&signal.envelope).unwrap();
            match envelope["payload"]["type"].as_str() {
                Some("answer") if signal.sequence > 0 => {
                    let sdp = envelope["payload"]["sdp"].as_str().unwrap().to_owned();
                    assert_ne!(sdp, initial_remote_sdp);
                    controller
                        .set_remote_description(RTCSessionDescription::answer(sdp).unwrap())
                        .await
                        .unwrap();
                    restart_answer_applied = true;
                }
                Some("ice-candidate") => pending_candidates.push(envelope),
                Some("ice-complete") => {}
                Some("answer") => {}
                other => panic!("unexpected host restart signal: {other:?}"),
            }
        }
        assert!(
            restart_answer_applied,
            "host did not publish an ICE restart answer"
        );
        for envelope in pending_candidates {
            controller
                .add_ice_candidate(RTCIceCandidateInit {
                    candidate: envelope["payload"]["candidate"]
                        .as_str()
                        .unwrap()
                        .to_owned(),
                    sdp_mid: envelope["payload"]["sdpMid"].as_str().map(str::to_owned),
                    sdp_mline_index: envelope["payload"]["sdpMLineIndex"]
                        .as_u64()
                        .map(|value| value as u16),
                    username_fragment: None,
                })
                .await
                .unwrap();
        }
        timeout(Duration::from_secs(10), async {
            while controller.connection_state() != RTCPeerConnectionState::Connected {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("controller did not reconnect after ICE restart");

        host.close().await.unwrap();
        controller.close().await.unwrap();
    }
}
