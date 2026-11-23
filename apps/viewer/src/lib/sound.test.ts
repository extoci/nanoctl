import { expect, test } from "bun:test";
import { SoundPlayback, setSoundEnabled, type SoundState } from "./sound";

test("sound requires explicit enable, toggles mute, and handles autoplay rejection", async () => {
  let blocked = false;
  let plays = 0;
  const audio = {
    muted: true,
    play: async () => {
      plays += 1;
      if (blocked) throw new Error("autoplay denied");
    },
  } as unknown as HTMLAudioElement;
  expect(audio.muted).toBe(true);
  expect(await setSoundEnabled(audio, true)).toBe(true);
  expect(audio.muted).toBe(false);
  expect(await setSoundEnabled(audio, false)).toBe(false);
  expect(audio.muted).toBe(true);
  expect(plays).toBe(1);
  blocked = true;
  expect(await setSoundEnabled(audio, true)).toBe(false);
  expect(audio.muted).toBe(true);
});

test("an early tap waits for the track, exposes a blocked attempt, and retries on tap", async () => {
  let blocked = true;
  let plays = 0;
  const states: SoundState[] = [];
  const audio = {
    srcObject: null as object | null,
    muted: true,
    play: async () => {
      plays += 1;
      if (blocked) throw new DOMException("Gesture required", "NotAllowedError");
    },
  };
  const sound = new SoundPlayback(audio as unknown as HTMLAudioElement, (state) =>
    states.push(state),
  );
  sound.toggle();
  expect(states).toEqual(["waiting"]);
  expect(audio.muted).toBe(true);
  expect(plays).toBe(0);
  audio.srcObject = {};
  sound.trackChanged();
  await Promise.resolve();
  await Promise.resolve();
  expect(states.at(-1)).toBe("blocked");
  expect(audio.muted).toBe(true);

  blocked = false;
  sound.toggle();
  expect(plays).toBe(2);
  expect(audio.muted).toBe(false);
  await Promise.resolve();
  await Promise.resolve();
  expect(states.at(-1)).toBe("playing");

  audio.srcObject = null;
  sound.trackChanged();
  expect(states.at(-1)).toBe("waiting");
  expect(audio.muted).toBe(true);
});

test("a delayed rejection from a canceled playback attempt cannot mute a new one", async () => {
  const first = Promise.withResolvers<void>();
  let plays = 0;
  const states: SoundState[] = [];
  const audio = {
    srcObject: {},
    muted: true,
    play: () => (++plays === 1 ? first.promise : Promise.resolve()),
  };
  const sound = new SoundPlayback(audio as unknown as HTMLAudioElement, (state) =>
    states.push(state),
  );
  sound.toggle();
  sound.toggle();
  expect(audio.muted).toBe(true);
  sound.toggle();
  first.reject(new DOMException("Playback interrupted", "AbortError"));
  await Promise.resolve();
  await Promise.resolve();
  expect(audio.muted).toBe(false);
  expect(states.at(-1)).toBe("playing");
});
