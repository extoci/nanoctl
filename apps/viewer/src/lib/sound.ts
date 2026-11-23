export async function setSoundEnabled(
  audio: HTMLAudioElement,
  enabled: boolean,
  isCurrent = () => true,
): Promise<boolean> {
  audio.muted = !enabled;
  if (!enabled) return false;
  try {
    await audio.play();
    return true;
  } catch {
    if (isCurrent()) audio.muted = true;
    return false;
  }
}

export type SoundState = "muted" | "waiting" | "starting" | "playing" | "blocked";

export class SoundPlayback {
  private state: SoundState = "muted";
  private attempt = 0;

  constructor(
    private readonly audio: HTMLAudioElement,
    private readonly onState: (state: SoundState) => void,
  ) {}

  toggle(): void {
    if (this.state === "muted" || this.state === "blocked") {
      this.play();
    } else {
      ++this.attempt;
      void setSoundEnabled(this.audio, false);
      this.update("muted");
    }
  }

  trackChanged(): void {
    if (this.state !== "muted" && this.state !== "blocked") this.play();
  }

  private play(): void {
    const attempt = ++this.attempt;
    if (!this.audio.srcObject) {
      this.audio.muted = true;
      this.update("waiting");
      return;
    }
    this.update("starting");
    const current = () => attempt === this.attempt;
    void setSoundEnabled(this.audio, true, current).then((playing) => {
      if (current()) this.update(playing ? "playing" : "blocked");
    });
  }

  private update(state: SoundState): void {
    this.state = state;
    this.onState(state);
  }
}
