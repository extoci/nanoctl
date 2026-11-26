//! Deterministic host-side session lifecycle and signaling validation.
//!
//! This module models ownership and ordering only. It does not perform WebRTC, socket, or timer I/O;
//! the host runtime executes returned commands and feeds asynchronous results back as events.

pub const MAX_ICE_GENERATION: u64 = 1_000_000;
const MAX_SDP_BYTES: usize = 1_000_000;
const MAX_CANDIDATE_BYTES: usize = 8_192;
const MAX_SESSION_TEXT_BYTES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControllerPayload {
    Offer {
        sdp: String,
    },
    IceCandidate {
        candidate: String,
        sdp_mid: Option<String>,
        sdp_mline_index: Option<u16>,
        username_fragment: Option<String>,
    },
    IceComplete,
    End {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerSignal {
    pub session_id: String,
    pub controller_epoch: String,
    pub host_epoch: String,
    pub generation: u64,
    pub sequence: u64,
    pub payload: ControllerPayload,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEvent {
    PeerConnected,
    PeerDisconnected,
    PeerFailed,
    RecoveryTimeout,
    SocketClosed,
    ReplacementRequested,
    TeardownComplete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnswerEvent {
    Created { generation: u64, sdp: String },
    Failed { generation: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionCommand {
    PrepareAnswer {
        generation: u64,
        offer_sdp: String,
        restart: bool,
    },
    SendAnswer {
        generation: u64,
        sequence: u64,
        host_epoch: String,
        sdp: String,
    },
    ApplyOrBufferIceCandidate {
        generation: u64,
        candidate: String,
        sdp_mid: Option<String>,
        sdp_mline_index: Option<u16>,
        username_fragment: Option<String>,
    },
    StartMedia,
    StopMedia,
    ClosePeer,
    SendTerminal {
        reason: TerminalReason,
    },
    CloseSocket,
    StartRecoveryTimer,
    CancelRecoveryTimer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalReason {
    RemoteEnd,
    Replaced,
    SocketClosed,
    RecoveryTimeout,
    NegotiationFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalDisposition {
    Applied,
    IgnoredStaleGeneration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalOutcome {
    pub disposition: SignalDisposition,
    pub commands: Vec<SessionCommand>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionError {
    WrongSession,
    WrongControllerEpoch,
    UnexpectedHostEpoch,
    InvalidSequence,
    InvalidGeneration,
    InvalidPayload(&'static str),
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lifecycle {
    Live,
    Closing,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SignalingPhase {
    AwaitingOffer,
    Answering,
    Established,
    Renegotiating,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionPhase {
    NotStarted,
    Connecting,
    Connected,
    Recovering,
}

/// Host-side owner for one signaling socket/session generation. Controller sequence numbers are
/// contiguous per session. ICE generations increase by one for each accepted restart offer.
#[derive(Debug)]
pub struct SessionMachine {
    session_id: String,
    host_epoch: String,
    controller_epoch: Option<String>,
    controller_sequence: u64,
    host_sequence: u64,
    ice_generation: u64,
    lifecycle: Lifecycle,
    signaling: SignalingPhase,
    connection: ConnectionPhase,
    peer_exists: bool,
    media_running: bool,
    recovery_timer_running: bool,
}

impl SessionMachine {
    pub fn new(session_id: impl Into<String>, host_epoch: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            host_epoch: host_epoch.into(),
            controller_epoch: None,
            controller_sequence: 0,
            host_sequence: 0,
            ice_generation: 0,
            lifecycle: Lifecycle::Live,
            signaling: SignalingPhase::AwaitingOffer,
            connection: ConnectionPhase::NotStarted,
            peer_exists: false,
            media_running: false,
            recovery_timer_running: false,
        }
    }

    pub fn is_closed(&self) -> bool {
        self.lifecycle == Lifecycle::Closed
    }

    pub fn handle_signal(
        &mut self,
        signal: &ControllerSignal,
    ) -> Result<SignalOutcome, SessionError> {
        if self.lifecycle != Lifecycle::Live {
            return Err(SessionError::Closed);
        }
        self.validate_identity(signal)?;
        match &signal.payload {
            ControllerPayload::Offer { sdp } => self.handle_offer(signal, sdp),
            ControllerPayload::IceCandidate {
                candidate,
                sdp_mid,
                sdp_mline_index,
                username_fragment,
            } => self.handle_candidate(
                signal,
                candidate,
                sdp_mid.clone(),
                *sdp_mline_index,
                username_fragment.clone(),
            ),
            ControllerPayload::IceComplete => self.handle_ice_complete(signal),
            ControllerPayload::End { .. } => {
                self.accept_controller_sequence(signal.sequence)?;
                Ok(SignalOutcome {
                    disposition: SignalDisposition::Applied,
                    commands: self.begin_close(TerminalReason::RemoteEnd, true),
                })
            }
        }
    }

    pub fn handle_answer_event(&mut self, event: AnswerEvent) -> Vec<SessionCommand> {
        if self.lifecycle != Lifecycle::Live {
            return Vec::new();
        }
        match event {
            AnswerEvent::Created { generation, sdp } => {
                let expected = self.ice_generation;
                let signaling_matches = match self.signaling {
                    SignalingPhase::Answering | SignalingPhase::Renegotiating => {
                        generation == expected
                    }
                    _ => false,
                };
                if !signaling_matches || sdp.is_empty() || sdp.len() > MAX_SDP_BYTES {
                    return Vec::new();
                }
                self.signaling = SignalingPhase::Established;
                if self.connection == ConnectionPhase::NotStarted {
                    self.connection = ConnectionPhase::Connecting;
                }
                self.peer_exists = true;
                self.host_sequence += 1;
                vec![SessionCommand::SendAnswer {
                    generation,
                    sequence: self.host_sequence,
                    host_epoch: self.host_epoch.clone(),
                    sdp,
                }]
            }
            AnswerEvent::Failed { generation } => {
                let active_answer = match self.signaling {
                    SignalingPhase::Answering | SignalingPhase::Renegotiating => {
                        generation == self.ice_generation
                    }
                    _ => false,
                };
                if active_answer {
                    self.begin_close(TerminalReason::NegotiationFailed, true)
                } else {
                    Vec::new()
                }
            }
        }
    }

    pub fn handle_event(&mut self, event: SessionEvent) -> Vec<SessionCommand> {
        if self.lifecycle == Lifecycle::Closed {
            return Vec::new();
        }
        match event {
            SessionEvent::PeerConnected => {
                if self.lifecycle != Lifecycle::Live
                    || self.connection == ConnectionPhase::NotStarted
                {
                    return Vec::new();
                }
                let mut commands = Vec::new();
                if self.recovery_timer_running {
                    self.recovery_timer_running = false;
                    commands.push(SessionCommand::CancelRecoveryTimer);
                }
                self.connection = ConnectionPhase::Connected;
                if !self.media_running {
                    self.media_running = true;
                    commands.push(SessionCommand::StartMedia);
                }
                commands
            }
            SessionEvent::PeerDisconnected | SessionEvent::PeerFailed => {
                if self.lifecycle != Lifecycle::Live
                    || !matches!(
                        self.connection,
                        ConnectionPhase::Connecting | ConnectionPhase::Connected
                    )
                {
                    return Vec::new();
                }
                self.connection = ConnectionPhase::Recovering;
                if self.recovery_timer_running {
                    Vec::new()
                } else {
                    self.recovery_timer_running = true;
                    vec![SessionCommand::StartRecoveryTimer]
                }
            }
            SessionEvent::RecoveryTimeout => {
                if self.lifecycle == Lifecycle::Live
                    && self.connection == ConnectionPhase::Recovering
                {
                    self.begin_close(TerminalReason::RecoveryTimeout, true)
                } else {
                    Vec::new()
                }
            }
            SessionEvent::SocketClosed => {
                if self.lifecycle == Lifecycle::Live {
                    self.begin_close(TerminalReason::SocketClosed, false)
                } else {
                    Vec::new()
                }
            }
            SessionEvent::ReplacementRequested => {
                if self.lifecycle == Lifecycle::Live {
                    self.begin_close(TerminalReason::Replaced, true)
                } else {
                    Vec::new()
                }
            }
            SessionEvent::TeardownComplete => {
                if self.lifecycle == Lifecycle::Closing {
                    self.lifecycle = Lifecycle::Closed;
                    self.connection = ConnectionPhase::NotStarted;
                }
                Vec::new()
            }
        }
    }

    fn validate_identity(&self, signal: &ControllerSignal) -> Result<(), SessionError> {
        if signal.session_id != self.session_id {
            return Err(SessionError::WrongSession);
        }
        if signal.session_id.is_empty() || signal.session_id.len() > MAX_SESSION_TEXT_BYTES {
            return Err(SessionError::WrongSession);
        }
        if !signal.host_epoch.is_empty() {
            return Err(SessionError::UnexpectedHostEpoch);
        }
        if signal.controller_epoch.is_empty()
            || signal.controller_epoch.len() > MAX_SESSION_TEXT_BYTES
        {
            return Err(SessionError::WrongControllerEpoch);
        }
        match &self.controller_epoch {
            Some(expected) if expected != &signal.controller_epoch => {
                Err(SessionError::WrongControllerEpoch)
            }
            None if !matches!(signal.payload, ControllerPayload::Offer { .. }) => {
                Err(SessionError::WrongControllerEpoch)
            }
            _ => Ok(()),
        }
    }

    fn handle_offer(
        &mut self,
        signal: &ControllerSignal,
        sdp: &str,
    ) -> Result<SignalOutcome, SessionError> {
        if sdp.is_empty() || sdp.len() > MAX_SDP_BYTES {
            return Err(SessionError::InvalidPayload("invalid offer SDP size"));
        }
        if signal.generation > MAX_ICE_GENERATION {
            return Err(SessionError::InvalidGeneration);
        }
        match self.signaling {
            SignalingPhase::AwaitingOffer => {
                if signal.generation != 0 || signal.sequence != 1 {
                    return Err(SessionError::InvalidSequence);
                }
                self.accept_controller_sequence(signal.sequence)?;
                self.controller_epoch = Some(signal.controller_epoch.clone());
                self.signaling = SignalingPhase::Answering;
                Ok(SignalOutcome {
                    disposition: SignalDisposition::Applied,
                    commands: vec![SessionCommand::PrepareAnswer {
                        generation: 0,
                        offer_sdp: sdp.to_owned(),
                        restart: false,
                    }],
                })
            }
            SignalingPhase::Established => {
                if signal.generation <= self.ice_generation {
                    self.accept_controller_sequence(signal.sequence)?;
                    return Ok(SignalOutcome {
                        disposition: SignalDisposition::IgnoredStaleGeneration,
                        commands: Vec::new(),
                    });
                }
                if signal.generation != self.ice_generation + 1 {
                    return Err(SessionError::InvalidGeneration);
                }
                self.accept_controller_sequence(signal.sequence)?;
                self.ice_generation = signal.generation;
                self.signaling = SignalingPhase::Renegotiating;
                Ok(SignalOutcome {
                    disposition: SignalDisposition::Applied,
                    commands: vec![SessionCommand::PrepareAnswer {
                        generation: signal.generation,
                        offer_sdp: sdp.to_owned(),
                        restart: true,
                    }],
                })
            }
            SignalingPhase::Answering | SignalingPhase::Renegotiating => {
                Err(SessionError::InvalidPayload("overlapping offer"))
            }
        }
    }

    fn handle_candidate(
        &mut self,
        signal: &ControllerSignal,
        candidate: &str,
        sdp_mid: Option<String>,
        sdp_mline_index: Option<u16>,
        username_fragment: Option<String>,
    ) -> Result<SignalOutcome, SessionError> {
        if self.signaling == SignalingPhase::AwaitingOffer {
            return Err(SessionError::InvalidPayload("candidate before offer"));
        }
        if signal.generation < self.ice_generation {
            self.accept_controller_sequence(signal.sequence)?;
            return Ok(SignalOutcome {
                disposition: SignalDisposition::IgnoredStaleGeneration,
                commands: Vec::new(),
            });
        }
        if signal.generation != self.ice_generation {
            return Err(SessionError::InvalidGeneration);
        }
        if candidate.is_empty()
            || candidate.len() > MAX_CANDIDATE_BYTES
            || sdp_mid
                .as_ref()
                .is_some_and(|value| value.len() > MAX_SESSION_TEXT_BYTES)
            || username_fragment
                .as_ref()
                .is_some_and(|value| value.is_empty() || value.len() > MAX_SESSION_TEXT_BYTES)
        {
            return Err(SessionError::InvalidPayload("invalid ICE candidate"));
        }
        self.accept_controller_sequence(signal.sequence)?;
        Ok(SignalOutcome {
            disposition: SignalDisposition::Applied,
            commands: vec![SessionCommand::ApplyOrBufferIceCandidate {
                generation: signal.generation,
                candidate: candidate.to_owned(),
                sdp_mid,
                sdp_mline_index,
                username_fragment,
            }],
        })
    }

    fn handle_ice_complete(
        &mut self,
        signal: &ControllerSignal,
    ) -> Result<SignalOutcome, SessionError> {
        if self.signaling == SignalingPhase::AwaitingOffer {
            return Err(SessionError::InvalidPayload("ICE completion before offer"));
        }
        if signal.generation < self.ice_generation {
            self.accept_controller_sequence(signal.sequence)?;
            return Ok(SignalOutcome {
                disposition: SignalDisposition::IgnoredStaleGeneration,
                commands: Vec::new(),
            });
        }
        if signal.generation != self.ice_generation {
            return Err(SessionError::InvalidGeneration);
        }
        self.accept_controller_sequence(signal.sequence)?;
        Ok(SignalOutcome {
            disposition: SignalDisposition::Applied,
            commands: Vec::new(),
        })
    }

    fn accept_controller_sequence(&mut self, sequence: u64) -> Result<(), SessionError> {
        if sequence != self.controller_sequence + 1 {
            return Err(SessionError::InvalidSequence);
        }
        self.controller_sequence = sequence;
        Ok(())
    }

    fn begin_close(&mut self, reason: TerminalReason, notify_remote: bool) -> Vec<SessionCommand> {
        if self.lifecycle != Lifecycle::Live {
            return Vec::new();
        }
        self.lifecycle = Lifecycle::Closing;
        let mut commands = Vec::new();
        if self.media_running {
            self.media_running = false;
            commands.push(SessionCommand::StopMedia);
        }
        if self.peer_exists {
            self.peer_exists = false;
            commands.push(SessionCommand::ClosePeer);
        }
        if notify_remote {
            commands.push(SessionCommand::SendTerminal { reason });
        }
        commands.push(SessionCommand::CloseSocket);
        commands
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine() -> SessionMachine {
        SessionMachine::new("session-1", "host-epoch")
    }

    fn offer(sequence: u64, generation: u64) -> ControllerSignal {
        ControllerSignal {
            session_id: "session-1".into(),
            controller_epoch: "controller-epoch".into(),
            host_epoch: String::new(),
            generation,
            sequence,
            payload: ControllerPayload::Offer {
                sdp: "offer".into(),
            },
        }
    }

    fn candidate(sequence: u64, generation: u64) -> ControllerSignal {
        ControllerSignal {
            payload: ControllerPayload::IceCandidate {
                candidate: "candidate:1".into(),
                sdp_mid: Some("0".into()),
                sdp_mline_index: Some(0),
                username_fragment: Some("ufrag".into()),
            },
            ..offer(sequence, generation)
        }
    }

    fn end(sequence: u64, generation: u64) -> ControllerSignal {
        ControllerSignal {
            payload: ControllerPayload::End {
                reason: "done".into(),
            },
            ..offer(sequence, generation)
        }
    }

    fn answer(machine: &mut SessionMachine, generation: u64) -> Vec<SessionCommand> {
        machine.handle_answer_event(AnswerEvent::Created {
            generation,
            sdp: "answer".into(),
        })
    }

    #[test]
    fn initial_offer_answer_and_connection_start_media_once() {
        let mut machine = machine();
        let outcome = machine.handle_signal(&offer(1, 0)).unwrap();
        assert_eq!(
            outcome.commands,
            vec![SessionCommand::PrepareAnswer {
                generation: 0,
                offer_sdp: "offer".into(),
                restart: false,
            }]
        );
        assert_eq!(
            answer(&mut machine, 0),
            vec![SessionCommand::SendAnswer {
                generation: 0,
                sequence: 1,
                host_epoch: "host-epoch".into(),
                sdp: "answer".into(),
            }]
        );
        assert_eq!(
            machine.handle_event(SessionEvent::PeerConnected),
            vec![SessionCommand::StartMedia]
        );
        assert!(machine.handle_event(SessionEvent::PeerConnected).is_empty());
    }

    #[test]
    fn first_signal_must_be_generation_zero_sequence_one_offer() {
        let mut machine = machine();
        assert_eq!(
            machine.handle_signal(&candidate(1, 0)),
            Err(SessionError::WrongControllerEpoch)
        );
        assert_eq!(
            machine.handle_signal(&offer(2, 0)),
            Err(SessionError::InvalidSequence)
        );
        assert_eq!(
            machine.handle_signal(&offer(1, 1)),
            Err(SessionError::InvalidSequence)
        );
    }

    #[test]
    fn sequences_must_be_contiguous() {
        let mut machine = machine();
        machine.handle_signal(&offer(1, 0)).unwrap();
        assert_eq!(
            machine.handle_signal(&candidate(3, 0)),
            Err(SessionError::InvalidSequence)
        );
        assert_eq!(
            machine.handle_signal(&candidate(1, 0)),
            Err(SessionError::InvalidSequence)
        );
        assert!(machine.handle_signal(&candidate(2, 0)).is_ok());
    }

    #[test]
    fn stale_generation_candidate_is_ignored_after_consuming_sequence() {
        let mut machine = machine();
        machine.handle_signal(&offer(1, 0)).unwrap();
        answer(&mut machine, 0);
        machine.handle_signal(&offer(2, 1)).unwrap();
        answer(&mut machine, 1);

        let outcome = machine.handle_signal(&candidate(3, 0)).unwrap();
        assert_eq!(
            outcome.disposition,
            SignalDisposition::IgnoredStaleGeneration
        );
        assert!(outcome.commands.is_empty());
        assert_eq!(
            machine.handle_signal(&candidate(3, 1)),
            Err(SessionError::InvalidSequence)
        );
        assert!(machine.handle_signal(&candidate(4, 1)).is_ok());
    }

    #[test]
    fn restart_offer_requires_exactly_next_generation_and_no_overlap() {
        let mut machine = machine();
        machine.handle_signal(&offer(1, 0)).unwrap();
        answer(&mut machine, 0);
        assert_eq!(
            machine.handle_signal(&offer(2, 2)),
            Err(SessionError::InvalidGeneration)
        );
        let restart = machine.handle_signal(&offer(2, 1)).unwrap();
        assert!(matches!(
            restart.commands.as_slice(),
            [SessionCommand::PrepareAnswer {
                restart: true,
                generation: 1,
                ..
            }]
        ));
        assert_eq!(
            machine.handle_signal(&offer(3, 2)),
            Err(SessionError::InvalidPayload("overlapping offer"))
        );
    }

    #[test]
    fn answer_from_wrong_generation_is_ignored() {
        let mut machine = machine();
        machine.handle_signal(&offer(1, 0)).unwrap();
        assert!(
            machine
                .handle_answer_event(AnswerEvent::Created {
                    generation: 99,
                    sdp: "stale".into(),
                })
                .is_empty()
        );
        assert_eq!(
            answer(&mut machine, 0),
            vec![SessionCommand::SendAnswer {
                generation: 0,
                sequence: 1,
                host_epoch: "host-epoch".into(),
                sdp: "answer".into(),
            }]
        );
    }

    #[test]
    fn disconnected_recovers_and_cancels_timer_on_reconnect() {
        let mut machine = machine();
        machine.handle_signal(&offer(1, 0)).unwrap();
        answer(&mut machine, 0);
        machine.handle_event(SessionEvent::PeerConnected);
        assert_eq!(
            machine.handle_event(SessionEvent::PeerDisconnected),
            vec![SessionCommand::StartRecoveryTimer]
        );
        assert!(
            machine
                .handle_event(SessionEvent::PeerDisconnected)
                .is_empty()
        );
        assert_eq!(
            machine.handle_event(SessionEvent::PeerConnected),
            vec![SessionCommand::CancelRecoveryTimer]
        );
    }

    #[test]
    fn recovery_timeout_closes_once_and_teardown_is_idempotent() {
        let mut machine = machine();
        machine.handle_signal(&offer(1, 0)).unwrap();
        answer(&mut machine, 0);
        machine.handle_event(SessionEvent::PeerConnected);
        machine.handle_event(SessionEvent::PeerFailed);
        let commands = machine.handle_event(SessionEvent::RecoveryTimeout);
        assert_eq!(
            commands,
            vec![
                SessionCommand::StopMedia,
                SessionCommand::ClosePeer,
                SessionCommand::SendTerminal {
                    reason: TerminalReason::RecoveryTimeout,
                },
                SessionCommand::CloseSocket,
            ]
        );
        assert!(
            machine
                .handle_event(SessionEvent::RecoveryTimeout)
                .is_empty()
        );
        assert!(
            machine
                .handle_event(SessionEvent::TeardownComplete)
                .is_empty()
        );
        assert!(machine.is_closed());
        assert_eq!(
            machine.handle_signal(&candidate(2, 0)),
            Err(SessionError::Closed)
        );
    }

    #[test]
    fn replacement_terminal_close_includes_replaced_reason() {
        let mut machine = machine();
        machine.handle_signal(&offer(1, 0)).unwrap();
        answer(&mut machine, 0);
        let commands = machine.handle_event(SessionEvent::ReplacementRequested);
        assert!(commands.contains(&SessionCommand::SendTerminal {
            reason: TerminalReason::Replaced,
        }));
        assert!(commands.contains(&SessionCommand::ClosePeer));
    }

    #[test]
    fn remote_end_uses_same_teardown_path() {
        let mut machine = machine();
        machine.handle_signal(&offer(1, 0)).unwrap();
        answer(&mut machine, 0);
        let outcome = machine.handle_signal(&end(2, 0)).unwrap();
        assert!(outcome.commands.contains(&SessionCommand::SendTerminal {
            reason: TerminalReason::RemoteEnd,
        }));
        assert!(outcome.commands.contains(&SessionCommand::CloseSocket));
    }

    #[test]
    fn wrong_identity_and_host_epoch_never_mutate_state() {
        let mut machine = machine();
        let mut wrong = offer(1, 0);
        wrong.session_id = "other".into();
        assert_eq!(
            machine.handle_signal(&wrong),
            Err(SessionError::WrongSession)
        );

        let mut host_epoch = offer(1, 0);
        host_epoch.host_epoch = "forged".into();
        assert_eq!(
            machine.handle_signal(&host_epoch),
            Err(SessionError::UnexpectedHostEpoch)
        );

        machine.handle_signal(&offer(1, 0)).unwrap();
        let mut wrong_epoch = candidate(2, 0);
        wrong_epoch.controller_epoch = "other".into();
        assert_eq!(
            machine.handle_signal(&wrong_epoch),
            Err(SessionError::WrongControllerEpoch)
        );
    }
}
