import { expect, test } from "bun:test";
import { setSoundEnabled } from "./sound";

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
