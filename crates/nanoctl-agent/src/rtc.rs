use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;
#[cfg(feature = "media")]
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use webrtc::api::APIBuilder;
use webrtc::api::media_engine::{MIME_TYPE_H264, MediaEngine};
#[cfg(feature = "media")]
use webrtc::data_channel::RTCDataChannel;
#[cfg(feature = "media")]
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::ice_transport::ice_candidate::{RTCIceCandidate, RTCIceCandidateInit};
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::policy::ice_transport_policy::RTCIceTransportPolicy;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use zeroize::Zeroizing;

use crate::control_plane::ControlPlane;

const PROTOCOL_VERSION: u8 = 1;

pub struct HostPeer {
    peer: Arc<RTCPeerConnection>,
    #[cfg_attr(not(feature = "media"), allow(dead_code))]
    video: Arc<TrackLocalStaticSample>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SignalEnvelope {
    version: u8,
    session_id: String,
    sender: String,
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
    },
    IceComplete,
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
        let offer = parse_offer(serialized_offer, &session_id)?;
        let mut media_engine = MediaEngine::default();
        media_engine.register_default_codecs()?;
        let api = APIBuilder::new().with_media_engine(media_engine).build();
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
            peer.on_data_channel(Box::new(move |channel: Arc<RTCDataChannel>| {
                let input = input.clone();
                if !matches!(
                    channel.label(),
                    "nanoctl.control.v1" | "nanoctl.pointer.v1"
                ) {
                    return Box::pin(async {});
                }
                let message_input = input.clone();
                channel.on_message(Box::new(move |message: DataChannelMessage| {
                    let input = message_input.clone();
                    Box::pin(async move {
                        if !message.is_string || input.is_none() {
                            return;
                        }
                        let result = tokio::task::spawn_blocking(move || {
                            input
                                .expect("input availability checked")
                                .lock()
                                .map_err(|_| anyhow::anyhow!("input controller lock poisoned"))?
                                .dispatch(&message.data)
                        })
                        .await;
                        if let Ok(Err(error)) = result {
                            tracing::warn!(error = %error, "control message rejected");
                        }
                    })
                }));
                channel.on_close(Box::new(move || {
                    let input = input.clone();
                    Box::pin(async move {
                        if let Some(input) = input {
                            if let Ok(mut input) = input.lock() {
                                input.release_all();
                            }
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
                sdp_fmtp_line:
                    "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f"
                        .to_owned(),
                rtcp_feedback: Vec::new(),
            },
            "desktop".to_owned(),
            "nanoctl".to_owned(),
        ));
        let sender = peer.add_track(video.clone()).await?;
        tokio::spawn(async move {
            let mut buffer = vec![0_u8; 1500];
            while sender.read(&mut buffer).await.is_ok() {}
        });

        let sequence = Arc::new(AtomicU64::new(1));
        peer.on_ice_candidate({
            let client = control_plane.clone();
            let token = token.clone();
            let session_id = session_id.clone();
            let sequence = sequence.clone();
            Box::new(move |candidate: Option<RTCIceCandidate>| {
                let client = client.clone();
                let token = token.clone();
                let session_id = session_id.clone();
                let sequence = sequence.clone();
                Box::pin(async move {
                    let payload = match candidate {
                        Some(candidate) => match candidate.to_json() {
                            Ok(candidate) => OutgoingPayload::IceCandidate {
                                candidate: candidate.candidate,
                                sdp_mid: candidate.sdp_mid,
                                sdp_mline_index: candidate.sdp_mline_index,
                            },
                            Err(_) => return,
                        },
                        None => OutgoingPayload::IceComplete,
                    };
                    let index = sequence.fetch_add(1, Ordering::Relaxed);
                    if let Ok(envelope) = serialize(&session_id, index, payload) {
                        let _ = client
                            .send_signal(&token, &session_id, index, &envelope)
                            .await;
                    }
                })
            })
        });

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
        control_plane
            .send_signal(&token, &session_id, 0, &envelope)
            .await?;
        Ok(Self { peer, video })
    }

    pub async fn add_signal(&self, serialized: &str, expected_session: &str) -> Result<bool> {
        let envelope: SignalEnvelope = serde_json::from_str(serialized)?;
        if envelope.version != PROTOCOL_VERSION
            || envelope.session_id != expected_session
            || envelope.sender != "controller"
        {
            anyhow::bail!("signal identity does not match the session");
        }
        match envelope.payload {
            SignalPayload::IceCandidate {
                candidate,
                sdp_mid,
                sdp_mline_index,
            } => {
                self.peer
                    .add_ice_candidate(RTCIceCandidateInit {
                        candidate,
                        sdp_mid,
                        sdp_mline_index,
                        username_fragment: None,
                    })
                    .await?;
                Ok(true)
            }
            SignalPayload::IceComplete => Ok(true),
            SignalPayload::End { reason } => {
                tracing::info!(reason = %reason, "controller ended session");
                self.peer.close().await?;
                Ok(false)
            }
            SignalPayload::Offer { .. } | SignalPayload::Unsupported => Ok(true),
        }
    }

    pub async fn close(&self) -> Result<()> {
        self.peer.close().await?;
        Ok(())
    }

    #[cfg(feature = "media")]
    pub fn video_track(&self) -> Arc<TrackLocalStaticSample> {
        self.video.clone()
    }
}

fn parse_offer(serialized: &str, expected_session: &str) -> Result<String> {
    if serialized.len() > 1_100_000 {
        anyhow::bail!("signal exceeds maximum size");
    }
    let envelope: SignalEnvelope =
        serde_json::from_str(serialized).context("offer envelope is invalid")?;
    if envelope.version != PROTOCOL_VERSION
        || envelope.session_id != expected_session
        || envelope.sender != "controller"
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_cross_session_offer() {
        let value = r#"{"version":1,"sessionId":"other","sequence":0,"sender":"controller","sentAt":1,"payload":{"type":"offer","sdp":"v=0"}}"#;
        assert!(parse_offer(value, "expected").is_err());
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
}
