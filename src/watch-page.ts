export function watchPage(controlEnabled: boolean): string {
  return `<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width,initial-scale=1" />
  <title>Nanoctl Viewer</title>
  <link rel="preconnect" href="https://fonts.googleapis.com" />
  <link rel="preconnect" href="https://fonts.gstatic.com" crossorigin />
  <link href="https://fonts.googleapis.com/css2?family=Geist+Mono:wght@400;500;700&display=swap" rel="stylesheet" />
  <style>
    :root {
      --bg: #050505;
      --bg-alt: #101416;
      --ink: #f7faf8;
      --muted: #9aa7ab;
      --line: #2a3438;
      --ok: #5af2a3;
      --warn: #ffcf66;
      --err: #ff7f7f;
      --accent: #d0ff71;
    }
    * { box-sizing: border-box; }
    html, body { width: 100%; height: 100%; }
    body {
      margin: 0;
      background:
        radial-gradient(circle at 20% 0%, rgba(208, 255, 113, .1), transparent 28%),
        linear-gradient(180deg, #040505 0%, #090d10 100%);
      overflow: hidden;
      color: var(--ink);
      font-family: "Geist Mono", monospace;
    }
    .frame { position: relative; width: 100vw; height: 100vh; }
    .chrome {
      position: absolute;
      inset: 0 0 auto 0;
      z-index: 5;
      height: 62px;
      border-bottom: 1px solid var(--line);
      background: rgba(5, 8, 10, .85);
      backdrop-filter: blur(6px);
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 1rem;
      padding: 0 1rem;
    }
    .id {
      font-size: .82rem;
      letter-spacing: .08em;
      text-transform: uppercase;
      color: var(--muted);
    }
    .status {
      display: inline-flex;
      align-items: center;
      gap: .55rem;
      font-size: .82rem;
      letter-spacing: .06em;
      text-transform: uppercase;
      color: var(--ink);
      border: 1px solid var(--line);
      padding: .46rem .6rem;
    }
    .dot {
      width: 10px;
      height: 10px;
      background: var(--muted);
      border-radius: 999px;
      animation: pulse 1.4s linear infinite;
    }
    .dot.connected { background: var(--ok); }
    .dot.connecting { background: var(--warn); }
    .dot.error { background: var(--err); }
    .actions { display: flex; gap: .52rem; align-items: center; }
    .action {
      border: 1px solid var(--line);
      background: #101416;
      color: var(--ink);
      font: 700 .78rem "Geist Mono", monospace;
      letter-spacing: .08em;
      text-transform: uppercase;
      padding: .55rem .72rem;
      cursor: pointer;
    }
    .action.primary {
      background: var(--accent);
      color: #09100b;
      border-color: var(--accent);
    }
    .viewport {
      width: 100vw;
      height: 100vh;
      padding-top: 62px;
      display: grid;
      place-items: center;
    }
    video {
      display: block;
      width: 100vw;
      height: calc(100vh - 62px);
      background: #000;
      object-fit: contain;
      cursor: ${controlEnabled ? "crosshair" : "default"};
      outline: none;
    }
    audio { display: none; }
    .hud {
      position: absolute;
      right: 1rem;
      bottom: 1rem;
      z-index: 4;
      max-width: min(86vw, 420px);
      padding: .8rem .9rem;
      border: 1px solid var(--line);
      background: rgba(10, 12, 15, .84);
      color: var(--muted);
      font-size: .74rem;
      line-height: 1.55;
    }
    .is-fullscreen .chrome,
    .is-fullscreen .hud {
      display: none;
    }
    .is-fullscreen .viewport { padding-top: 0; }
    .is-fullscreen video { height: 100vh; }
    @keyframes pulse {
      0%, 100% { opacity: .45; }
      50% { opacity: 1; }
    }
    @media (max-width: 640px) {
      .chrome {
        height: 70px;
        padding: 0 .7rem;
        gap: .6rem;
      }
      .viewport { padding-top: 70px; }
      video { height: calc(100vh - 70px); }
      .actions { gap: .4rem; }
      .action { font-size: .72rem; padding: .48rem .56rem; }
      .hud { left: .8rem; right: .8rem; bottom: .8rem; }
    }
  </style>
</head>
<body>
  <main class="frame">
    <header class="chrome">
      <div class="id">Nanoctl / Viewer</div>
      <div class="status">
        <span id="statusDot" class="dot connecting"></span>
        <span id="statusText">Connecting</span>
      </div>
      <div class="actions">
        <button id="controlBtn" class="action ${controlEnabled ? "primary" : ""}" type="button">${controlEnabled ? "Control On" : "Control Off"}</button>
        <button id="enableSoundBtn" class="action" type="button" hidden>Enable Sound</button>
        <button id="fullscreenBtn" class="action" type="button">Fullscreen</button>
      </div>
    </header>
    <section class="viewport">
      <video id="video" autoplay playsinline muted tabindex="0"></video>
      <audio id="audio" autoplay playsinline></audio>
    </section>
    <aside class="hud">
      ${controlEnabled ? "click to focus the host, move/click to control the pointer, and type for key presses. press F for fullscreen." : "view-only mode. remote control is disabled on the host."}
    </aside>
  </main>

  <script>
    const CONTROL_ENABLED = ${controlEnabled ? "true" : "false"};
    const video = document.getElementById("video");
    const audio = document.getElementById("audio");
    const statusDot = document.getElementById("statusDot");
    const statusText = document.getElementById("statusText");
    const fullscreenBtn = document.getElementById("fullscreenBtn");
    const enableSoundBtn = document.getElementById("enableSoundBtn");
    const controlBtn = document.getElementById("controlBtn");
    let peer = null;
    let controlChannel = null;
    let reconnectTimer = null;
    let audioBlocked = false;
    let controlActive = CONTROL_ENABLED;

    const configureReceiverForLowLatency = (receiver) => {
      if (!receiver) return;
      if ("playoutDelayHint" in receiver) receiver.playoutDelayHint = 0;
      if ("jitterBufferTarget" in receiver) receiver.jitterBufferTarget = 0;
    };

    const setStatus = (state, label) => {
      statusText.textContent = label;
      statusDot.className = "dot " + state;
    };

    const setAudioBlockedState = (blocked) => {
      audioBlocked = blocked;
      enableSoundBtn.hidden = !blocked;
      if (!blocked && peer && peer.connectionState === "connected") {
        setStatus("connected", "Live");
      }
      if (blocked && peer && peer.connectionState === "connected") {
        setStatus("connecting", "Live / Enable Sound");
      }
    };

    const attemptAudioPlayback = () => {
      const playPromise = audio.play();
      if (!playPromise || typeof playPromise.then !== "function") {
        setAudioBlockedState(false);
        return;
      }
      playPromise.then(() => setAudioBlockedState(false)).catch(() => setAudioBlockedState(true));
    };

    const closePeer = () => {
      if (!peer) return;
      try { peer.ontrack = null; } catch {}
      try { peer.close(); } catch {}
      peer = null;
      controlChannel = null;
      setAudioBlockedState(false);
    };

    const waitForIceGatheringComplete = (pc, timeoutMs = 1500) =>
      new Promise((resolve) => {
        if (pc.iceGatheringState === "complete") {
          resolve();
          return;
        }
        const timer = setTimeout(() => {
          pc.removeEventListener("icegatheringstatechange", onChange);
          resolve();
        }, timeoutMs);
        const onChange = () => {
          if (pc.iceGatheringState !== "complete") return;
          clearTimeout(timer);
          pc.removeEventListener("icegatheringstatechange", onChange);
          resolve();
        };
        pc.addEventListener("icegatheringstatechange", onChange);
      });

    const scheduleReconnect = () => {
      clearTimeout(reconnectTimer);
      setStatus("connecting", "Reconnecting");
      reconnectTimer = setTimeout(() => {
        connect().catch((err) => console.error(err));
      }, 700);
    };

    const sendControl = (payload) => {
      if (!controlActive) return;
      if (!controlChannel || controlChannel.readyState !== "open") return;
      controlChannel.send(JSON.stringify(payload));
    };

    const getNormalizedPoint = (event) => {
      const rect = video.getBoundingClientRect();
      const frameWidth = video.videoWidth || rect.width;
      const frameHeight = video.videoHeight || rect.height;
      const frameAspect = frameWidth / frameHeight;
      const boxAspect = rect.width / rect.height;
      let activeWidth = rect.width;
      let activeHeight = rect.height;
      let offsetLeft = 0;
      let offsetTop = 0;

      if (frameAspect > boxAspect) {
        activeHeight = rect.width / frameAspect;
        offsetTop = (rect.height - activeHeight) / 2;
      } else {
        activeWidth = rect.height * frameAspect;
        offsetLeft = (rect.width - activeWidth) / 2;
      }

      const rawX = (event.clientX - rect.left - offsetLeft) / activeWidth;
      const rawY = (event.clientY - rect.top - offsetTop) / activeHeight;
      return {
        x: Math.min(1, Math.max(0, rawX)),
        y: Math.min(1, Math.max(0, rawY))
      };
    };

    const pointerPayload = (event, action) => {
      const point = getNormalizedPoint(event);
      return {
        kind: "pointer",
        action,
        x: point.x,
        y: point.y,
        button: event.button
      };
    };

    const keyPayload = (event, action) => ({
      kind: "key",
      action,
      key: event.key
    });

    const connect = async () => {
      clearTimeout(reconnectTimer);
      closePeer();
      setStatus("connecting", "Connecting");

      const pc = new RTCPeerConnection({ iceServers: [] });
      peer = pc;
      pc.addTransceiver("video", { direction: "recvonly" });
      pc.addTransceiver("audio", { direction: "recvonly" });
      pc.getReceivers().forEach(configureReceiverForLowLatency);

      if (CONTROL_ENABLED) {
        controlChannel = pc.createDataChannel("control", { ordered: true });
      }

      pc.ontrack = (event) => {
        configureReceiverForLowLatency(event.receiver);
        const stream = event.streams && event.streams[0] ? event.streams[0] : new MediaStream([event.track]);
        if (video.srcObject !== stream) video.srcObject = stream;
        if (audio.srcObject !== stream) audio.srcObject = stream;
        video.play().catch(() => {});
        if (event.track.kind === "audio") attemptAudioPlayback();
      };

      pc.onconnectionstatechange = () => {
        const state = pc.connectionState;
        if (state === "connected") {
          setStatus(audioBlocked ? "connecting" : "connected", audioBlocked ? "Live / Enable Sound" : "Live");
          return;
        }
        if (state === "connecting") {
          setStatus("connecting", "Connecting");
          return;
        }
        if (state === "failed") setStatus("error", "Connection Failed");
        if (state === "failed" || state === "disconnected" || state === "closed") {
          scheduleReconnect();
        }
      };

      const offer = await pc.createOffer();
      await pc.setLocalDescription(offer);
      await waitForIceGatheringComplete(pc);

      const response = await fetch("/webrtc/offer", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(pc.localDescription)
      });

      if (!response.ok) {
        setStatus("error", "Signaling Error");
        throw new Error("Signaling failed (" + response.status + ")");
      }

      const answer = await response.json();
      await pc.setRemoteDescription(answer);
    };

    const toggleFullscreen = async () => {
      if (document.fullscreenElement) {
        await document.exitFullscreen();
        return;
      }
      const target = document.documentElement;
      if (target.requestFullscreen) await target.requestFullscreen();
    };

    const syncFullscreenButton = () => {
      const isFullscreen = Boolean(document.fullscreenElement);
      fullscreenBtn.textContent = isFullscreen ? "Exit Fullscreen" : "Fullscreen";
      document.body.classList.toggle("is-fullscreen", isFullscreen);
    };

    const syncControlButton = () => {
      controlBtn.textContent = controlActive ? "Control On" : "Control Off";
      controlBtn.classList.toggle("primary", controlActive);
    };

    fullscreenBtn.addEventListener("click", () => toggleFullscreen().catch(() => {}));
    enableSoundBtn.addEventListener("click", () => attemptAudioPlayback());
    controlBtn.addEventListener("click", () => {
      if (!CONTROL_ENABLED) return;
      controlActive = !controlActive;
      syncControlButton();
      if (controlActive) video.focus();
    });

    video.addEventListener("mousemove", (event) => sendControl(pointerPayload(event, "move")));
    video.addEventListener("mousedown", (event) => {
      event.preventDefault();
      video.focus();
      sendControl(pointerPayload(event, "down"));
    });
    video.addEventListener("mouseup", (event) => {
      event.preventDefault();
      sendControl(pointerPayload(event, "up"));
    });
    video.addEventListener("contextmenu", (event) => event.preventDefault());

    video.addEventListener("keydown", (event) => {
      if (event.key === "f" || event.key === "F") {
        if (!event.repeat && !event.metaKey && !event.ctrlKey && !event.altKey) {
          event.preventDefault();
          toggleFullscreen().catch(() => {});
        }
        return;
      }
      if (!CONTROL_ENABLED || !controlActive) return;
      event.preventDefault();
      if (event.repeat) return;
      sendControl(keyPayload(event, "press"));
    });

    document.addEventListener("fullscreenchange", syncFullscreenButton);
    syncFullscreenButton();
    syncControlButton();

    connect().catch((err) => {
      console.error(err);
      setStatus("error", "Connection Failed");
      scheduleReconnect();
    });

    window.addEventListener("beforeunload", () => {
      clearTimeout(reconnectTimer);
      closePeer();
    });
  </script>
</body>
</html>`;
}
