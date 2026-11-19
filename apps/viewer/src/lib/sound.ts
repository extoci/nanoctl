export async function setSoundEnabled(audio: HTMLAudioElement, enabled: boolean): Promise<boolean> {
  audio.muted = !enabled;
  if (!enabled) return false;
  try {
    await audio.play();
    return true;
  } catch {
    audio.muted = true;
    return false;
  }
}
