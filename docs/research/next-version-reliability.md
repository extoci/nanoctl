# Next-version reliability research

_Research date: 2026-08-04._

This note uses the W3C WebRTC specification, first-party MDN/WebRTC material, Microsoft documentation, and pinned Moonlight/Sunshine/WebRTC source. Recommendations are marked as such; observations about nanoctl are from the current checkout.

## Highest-confidence recommendations

### 1. Make every WebRTC session a generation-owned resource

Use one lifecycle owner per session generation. On replacement or terminal failure, mark the generation closed first, stop accepting its signals, serialize teardown, and only then create a new `RTCPeerConnection`. The WebRTC specification says `close()` terminates the ICE agent, closes the signaling state, stops transceivers, closes data channels, tears down DTLS/SCTP, and releases TURN permissions. [W3C `RTCPeerConnection.close()`](https://www.w3.org/TR/webrtc/#dom-peerconnection-close) MDN also explicitly recommends removing references to the old connection before creating another one for the same peer. [MDN `RTCPeerConnection.close()`](https://developer.mozilla.org/en-US/docs/Web/API/RTCPeerConnection/close)

Recommended teardown order:

1. Increment or invalidate a session-generation token and cancel queued signaling work.
2. Remove peer event handlers and reject/ignore callbacks carrying the old token.
3. Release input ownership, stop local/remote tracks, close data channels, and close the peer connection.
4. Abort capture/encoder tasks on the host, clear timers and queues, and drop all references.
5. Construct the replacement peer only after the old teardown has completed.

This makes cleanup idempotent and prevents a delayed candidate, timer, or promise continuation from mutating a replacement peer. It also gives the host and viewer the same terminal rule: after the retry budget is exhausted, send one terminal signal and close all generation-owned resources.

Do not treat every ICE `disconnected` event as terminal. The W3C state model describes `disconnected` as potentially transient, while `failed` is terminal until an ICE restart succeeds; MDN likewise cautions against closing immediately on `disconnected`. [W3C ICE connection state](https://www.w3.org/TR/webrtc/#dom-rtciceconnectionstate), [MDN signaling and video calling](https://developer.mozilla.org/en-US/docs/Web/API/WebRTC_API/Signaling_and_video_calling)

### 2. Serialize negotiation, and make ICE candidates generation-aware

For the controller-offers model, keep all `setLocalDescription`, `setRemoteDescription`, and `addIceCandidate` operations on one per-session queue. The specification defines these as asynchronous operations with ordered processing, and `addIceCandidate()` fails when there is no remote description. [W3C `setLocalDescription()`](https://www.w3.org/TR/webrtc/#dom-peerconnection-setlocaldescription), [W3C `setRemoteDescription()`](https://www.w3.org/TR/webrtc/#dom-peerconnection-setremotedescription), [W3C `addIceCandidate()`](https://www.w3.org/TR/webrtc/#dom-peerconnection-addicecandidate)

The offer path should be:

1. Wait until the peer is in the expected signaling state and no offer/restart is in flight.
2. `await pc.setLocalDescription()` (or set a created offer).
3. Read and signal `pc.localDescription` only after that promise fulfills.
4. Send trickle candidates from `icecandidate` events through the same session/generation guard.

MDN's perfect-negotiation pattern uses the fulfilled `localDescription` and an explicit `makingOffer` guard because signaling-state changes are asynchronous. Even without offer glare in the current controller-only design, the guard and queue are valuable for reconnects and ICE restarts. [MDN perfect negotiation](https://developer.mozilla.org/en-US/docs/Web/API/WebRTC_API/Perfect_negotiation)

Treat each ICE restart as a new candidate generation:

- Include the candidate's `usernameFragment` (ICE ufrag) in the signaling envelope, and retain the generation identifier with the offer/answer.
- Buffer candidates until the remote description for that generation is installed; then apply them in sequence. Reject or discard candidates from an older generation rather than applying them to the newest description.
- Signal end-of-candidates for the matching generation and media section. The specification distinguishes an empty candidate (end for a generation/m-line) from a `null` candidate (all transports complete, and legacy behavior); a ufrag identifies which ICE generation a candidate belongs to. [W3C `addIceCandidate()`](https://www.w3.org/TR/webrtc/#dom-peerconnection-addicecandidate), [MDN `addIceCandidate()`](https://developer.mozilla.org/en-US/docs/Web/API/RTCPeerConnection/addIceCandidate)
- Start a restart only from a stable, live session, keep one restart in flight, and use a bounded recovery timer for `disconnected`. `restartIce()` marks the next offer for new ICE credentials; `createOffer({ iceRestart: true })` is also supported by the API. [W3C `restartIce()`](https://www.w3.org/TR/webrtc/#dom-peerconnection-restartice)

For nanoctl, the current `ice-complete` envelope can remain as a compatibility signal, but it should carry the ICE generation/ufrag and be handled as a real end-of-candidates event rather than as a host-side no-op. Sequence numbers order delivery; they do not by themselves prevent an old-generation candidate from reaching a replacement peer.

### 3. Use an interactive per-user Windows task for desktop capture

Windows services run in session 0 and cannot directly interact with a user's desktop on modern Windows; Microsoft advises against interactive services for new code. [Microsoft: Interactive Services](https://learn.microsoft.com/en-us/windows/win32/services/interactive-services) A user-specific logon-triggered scheduled task is the appropriate boundary for a capture agent that needs the logged-in user's graphics/input context. Microsoft documents a logon trigger with a user ID, and the Task Scheduler protocol defines `TASK_LOGON_INTERACTIVE_TOKEN` as running in the user's interactive logon session. [Microsoft: starting an executable at logon](https://learn.microsoft.com/en-us/windows/win32/taskschd/starting-an-executable-when-a-user-logs-on), [Microsoft Task Scheduler logon type](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-tsch/6daca1c9-6766-46f4-9378-5f5a9260c967)

Recommended task properties:

- Direct executable action, scoped to the exact user and configured with an interactive token.
- Limited run level unless a narrowly justified capture permission requires elevation. [Microsoft `New-ScheduledTaskPrincipal`](https://learn.microsoft.com/en-us/powershell/module/scheduledtasks/new-scheduledtaskprincipal?view=windowsserver2025-ps)
- Hidden task metadata, no execution time limit, and restart-on-failure with a finite count and interval. `Hidden` controls Task Scheduler visibility; it does not suppress a console created by a console-subsystem executable. [Microsoft task settings](https://learn.microsoft.com/en-us/windows/win32/taskschd/taskschedulerschema-settings-tasktype-element), [Microsoft restart-on-failure](https://learn.microsoft.com/en-us/windows/win32/taskschd/taskschedulerschema-restartonfailure-settingstype-element), [Microsoft execution time limit](https://learn.microsoft.com/en-us/windows/win32/taskschd/tasksettings-executiontimelimit)
- For a no-console capture process, build the Windows agent as a Windows-subsystem executable, or have a small trusted launcher create the console child with `CREATE_NO_WINDOW`. Keep diagnostics in a file/Event Log or a separate console-oriented debug binary. [Microsoft `/SUBSYSTEM`](https://learn.microsoft.com/en-us/cpp/build/reference/subsystem?view=msvc-170), [Microsoft process creation flags](https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags)

If a system service is later needed for machine-wide orchestration, keep capture in a per-user worker. Microsoft documents obtaining a logged-on user's token and creating a process in that session, but that path requires a highly trusted service and additional session/credential handling. [Microsoft `WTSQueryUserToken`](https://learn.microsoft.com/en-us/windows/win32/api/wtsapi32/nf-wtsapi32-wtsqueryusertoken) It is more complex than the current per-user task and should not be introduced solely to launch capture.

### 4. Make upgrades transactional and health-gated

The high-confidence update shape is:

1. Stage outside the active install directory; verify the signed manifest, hash, and size.
2. Acquire an update lock, stop the scheduled task, and wait for the process and its capture/encoder resources to exit.
3. Replace the executable on the same volume using `ReplaceFile` with a backup, or an equivalent explicit rename transaction that never discards the known-good binary before the candidate is safely installed. Microsoft documents `ReplaceFile` as an atomic-style replacement operation with an optional backup and same-volume requirements. [Microsoft `ReplaceFile`](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-replacefilea)
4. Start the task and verify the _running candidate_ reaches a readiness condition: process alive, capture initialized, signaling endpoint available, and a short-lived health/heartbeat check succeeds.
5. Keep the backup through a stability window. If startup or health fails, stop the candidate and restore the backup; retain failed artifacts and recovery metadata if rollback also fails.

This is an inference from the Windows replacement and restart model: the updater must be a separate process because the running executable cannot safely replace itself, and a post-start process exit is not the same as a successful `doctor` invocation. Windows Restart Manager is available when locked resources require coordinated application shutdown/restart. [Microsoft Restart Manager](https://learn.microsoft.com/en-us/windows/win32/rstmgr/about-restart-manager)

The current `update-agent.ps1` already has the important signed-staging, stop/wait, candidate/previous, and rollback shape. The next version should add the update lock and a readiness/stability gate before deleting `.previous`; otherwise a task that starts and then immediately fails can be committed as healthy.

### 5. Pace from capture time, keep only useful frames, and configure for low delay

Use an event-driven capture-to-encode handoff with capacity one: if the encoder is busy, replace the queued frame with the newest frame and count the drop. Sunshine's Windows Graphics Capture implementation uses a two-buffer frame pool, records the system-relative capture timestamp, and keeps only the newest produced frame. [Sunshine Windows Graphics Capture](https://github.com/LizardByte/Sunshine/blob/0784774fecb4ffcd7ff1bf1c26bba84af516590e/src/platform/windows/display_wgc.cpp#L141), [Sunshine video pipeline](https://github.com/LizardByte/Sunshine/blob/0784774fecb4ffcd7ff1bf1c26bba84af516590e/src/video.cpp#L1513)

Carry a monotonic capture timestamp into the encoded sample and derive RTP/PTS from that clock. Google WebRTC's `VideoStreamEncoder` normalizes capture timestamps, maps them to a 90-kHz RTP clock, and drops frames whose capture timestamp is not newer or whose encoder is already blocked. [WebRTC `VideoStreamEncoder`](https://webrtc.googlesource.com/src/+/7a9a092708f1f3abc45f9aabda2db205132cc4ac/video/video_stream_encoder.cc#L1061)

For static content, use a bounded minimum refresh/encode cadence rather than an unbounded poll or a bursty catch-up loop. Sunshine waits for a frame only up to a maximum frame time and then encodes again to avoid stalls on unchanged content. [Sunshine encode loop](https://github.com/LizardByte/Sunshine/blob/0784774fecb4ffcd7ff1bf1c26bba84af516590e/src/video.cpp#L2386) If nanoctl retains an explicit target-FPS scheduler, advance an absolute monotonic deadline and skip missed deadlines; never emit catch-up bursts, and do not replace source timestamps with encoder-completion time.

Configure the hardware encoder for the latency budget: no lookahead, no frame reordering, low-delay keyframes, CBR, and a small VBV buffer are the relevant Sunshine NVENC choices. [Sunshine NVENC rate control](https://github.com/LizardByte/Sunshine/blob/0784774fecb4ffcd7ff1bf1c26bba84af516590e/src/nvenc/nvenc_base.cpp#L255) On the client, keep at most the buffering needed for the selected policy: Moonlight documents immediate rendering/drop behavior for lowest latency, roughly one frame of buffering for balanced pacing, and unbounded growth only for a smoothness-first mode that can accumulate latency. [Moonlight frame-pacing FAQ](https://github.com/moonlight-stream/moonlight-docs/wiki/Frequently-Asked-Questions)

After capture size/format changes or encoder recovery, force a clean reconfiguration and an IDR before resuming normal frames. Track capture-to-encode delay, encoder queue depth, frame drops, packet loss/RTT, decode queue depth, and glass-to-glass latency; these counters distinguish a pacing problem from a network or encoder problem.

## Applied to the current checkout

The repository already has several good foundations: one active session, bounded latest-frame capture, low-latency H.264 settings, periodic IDR intent, per-user interactive task installation, signed staged updates, and explicit cleanup paths.

The highest-value next changes are:

- Make teardown and inbound signaling generation-aware; ensure terminal state calls the same full cleanup as unmount.
- Signal `pc.localDescription` after `setLocalDescription()` completes, serialize restarts, and add ICE ufrag/generation handling plus candidate buffering.
- Keep the per-user task model, but remove console creation at the executable/launcher level; task hiding alone is insufficient.
- Add a post-start health/stability gate and an update lock before deleting the previous binary.
- Attach capture timestamps to samples, preserve newest-frame-only behavior, and make frame duration/RTP timestamps reflect a monotonic source clock.

These recommendations are deliberately limited to lifecycle, Windows startup/update safety, and media pacing; no application code was changed as part of this research.
