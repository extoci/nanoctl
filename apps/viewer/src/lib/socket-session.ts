import type { CodecChoice } from "./video-codec";
import {
  parseAgentMessage,
  assertHostInfo,
  type HostInfo,
  type AgentClientMessage,
} from "@nanoctl/protocol";
import {
  RemoteSessionController,
  createControllerEpoch,
  type RemoteSessionSnapshot,
  type RemoteSessionOptions,
} from "./remote-session";

export type SocketStatus =
  | "connecting"
  | "online"
  | "reconnecting"
  | "replaced"
  | "disconnected"
  | "error";
export const reconnectDelay = (attempt: number): number | null =>
  attempt >= 5 ? null : Math.min(500 * 2 ** attempt, 8_000);

export class SocketSession {
  controller: RemoteSessionController | null = null;
  private socket: WebSocket | null = null;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private abort: AbortController | null = null;
  private revision = 0;
  private attempt = 0;
  private disposed = false;
  private readonly preferences = { inputEnabled: true };
  private readonly restored = (event: PageTransitionEvent) => {
    if (event.persisted) this.connect();
  };

  constructor(
    private readonly options: {
      video: HTMLVideoElement;
      audio?: HTMLAudioElement;
      onAudioTrackChange?: () => void;
      onInfo: (info: HostInfo) => void;
      onSnapshot: (snapshot: RemoteSessionSnapshot) => void;
      onStatus: (status: SocketStatus, detail?: string) => void;
      createController?: (options: RemoteSessionOptions) => RemoteSessionController;
      codecChoice?: () => CodecChoice;
    },
  ) {
    window.addEventListener("pageshow", this.restored);
  }

  connect(): void {
    if (this.disposed) return;
    this.attempt = 0;
    this.open();
  }

  private clear(): void {
    this.revision += 1;
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = null;
    this.abort?.abort();
    this.abort = null;
    const socket = this.socket;
    this.socket = null;
    if (socket) {
      socket.onmessage = null;
      socket.onclose = null;
      socket.onerror = null;
      socket.onopen = null;
      socket.close();
    }
    const controller = this.controller;
    this.controller = null;
    void controller?.stop("socket closed", false);
  }

  private open(): void {
    this.clear();
    const revision = this.revision;
    const current = () => !this.disposed && revision === this.revision;
    this.options.onStatus(this.attempt ? "reconnecting" : "connecting");
    const abort = new AbortController();
    this.abort = abort;
    this.timer = setTimeout(() => {
      if (current()) this.retry("Host did not respond");
    }, 15_000);
    void fetch("/v1/info", { cache: "no-store", signal: abort.signal })
      .then(async (response) => {
        if (!response.ok) {
          const detail = await response.text();
          if (current() && response.status === 403) {
            this.terminal("error", detail);
            return;
          }
          throw new Error(detail || `Host returned ${response.status}`);
        }
        const info: unknown = await response.json();
        assertHostInfo(info);
        if (!current()) return;
        this.options.onInfo(info);
        const url = new URL("/v1/connect", window.location.href);
        url.protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
        const socket = new WebSocket(url);
        this.socket = socket;
        let welcomed = false;
        let localFailure: string | undefined;
        socket.onmessage = (event: MessageEvent<string>) => {
          if (!current()) return;
          try {
            const message = parseAgentMessage(JSON.parse(event.data));
            if (message.type === "end") {
              if (message.reason === "replaced") {
                this.terminal("replaced", "Another viewer took over");
              } else if (message.reason === "ended by controller") {
                this.terminal("error", message.reason);
              } else {
                this.retry(localFailure ?? message.reason);
              }
              return;
            }
            if (message.type === "hello") {
              if (welcomed) throw new Error("Duplicate server hello");
              welcomed = true;
              if (this.timer !== null) clearTimeout(this.timer);
              this.timer = null;
              this.options.onInfo(message.host);
              this.options.onStatus("online");
              const createController =
                this.options.createController ??
                ((options) => new RemoteSessionController(options));
              const controller = createController({
                sessionId: message.sessionId,
                controllerEpoch: createControllerEpoch(),
                displays: message.host.displays,
                iceServers: [],
                video: this.options.video,
                audio: this.options.audio,
                onAudioTrackChange: this.options.onAudioTrackChange,
                systemAudioEnabled: message.host.capabilities.systemAudio,
                remoteInputEnabled: message.host.remoteInput,
                remoteInputPolicy: message.host.viewer.mayControl,
                gamepadAvailable: message.host.capabilities.gamepad === true,
                clipboardEnabled:
                  message.host.capabilities.clipboard &&
                  message.host.remoteInput &&
                  message.host.viewer.mayControl,
                preferences: this.preferences,
                videoCodecs: message.host.capabilities.codecs,
                codecChoice: this.options.codecChoice?.(),
                operations: {
                  sendSignal: async ({ envelope }) =>
                    this.send(socket, {
                      type: "signal",
                      envelope: JSON.parse(envelope),
                    }),
                  endSession: async ({ reason }) => {
                    if (current()) {
                      localFailure = reason;
                      this.send(socket, { type: "end", reason });
                      this.timer = setTimeout(() => {
                        if (current()) {
                          if (reason === "ended by controller") this.terminal("error", reason);
                          else this.retry(reason);
                        }
                      }, 5_000);
                    }
                  },
                },
                onSnapshot: (snapshot) => {
                  if (!current()) return;
                  if (snapshot.phase === "connected") this.attempt = 0;
                  this.options.onSnapshot(snapshot);
                },
              });
              this.controller = controller;
              void controller.start().catch(() => {
                if (current()) this.terminal("error", "WebRTC is unavailable in this browser");
              });
            } else {
              if (!welcomed || !this.controller) throw new Error("Signal arrived before hello");
              this.controller.ingestHostSignals([
                {
                  sequence: message.envelope.sequence,
                  envelope: JSON.stringify(message.envelope),
                },
              ]);
            }
          } catch {
            this.terminal("error", "The host sent an invalid signaling message");
          }
        };
        socket.onclose = () => {
          if (current()) this.retry("Connection to the host was lost");
        };
        socket.onerror = () => {
          if (current()) this.retry("Cannot open the host signaling connection");
        };
      })
      .catch((error: unknown) => {
        if (current()) this.retry(error instanceof Error ? error.message : "Host unavailable");
      });
  }

  private send(socket: WebSocket, message: AgentClientMessage): void {
    const text = JSON.stringify(message);
    if (new TextEncoder().encode(text).byteLength > 64 * 1024)
      throw new Error("Signal exceeds 64 KiB");
    if (
      socket !== this.socket ||
      socket.readyState !== WebSocket.OPEN ||
      socket.bufferedAmount > 256 * 1024
    ) {
      throw new Error("Signaling connection unavailable");
    }
    socket.send(text);
  }

  private retry(detail: string): void {
    const delay = reconnectDelay(this.attempt++);
    this.clear();
    if (delay === null) {
      this.options.onStatus("error", `${detail}. Reconnect to try again.`);
      return;
    }
    this.options.onStatus("reconnecting", detail);
    this.timer = setTimeout(() => this.open(), delay);
  }

  private terminal(status: SocketStatus, detail: string): void {
    this.clear();
    this.options.onStatus(status, detail);
  }

  disconnect(): void {
    this.terminal("disconnected", "You ended the session.");
  }

  dispose(): void {
    this.disposed = true;
    this.clear();
    window.removeEventListener("pageshow", this.restored);
  }
}
