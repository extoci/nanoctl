import { readMarkerRow } from "../../../tests/browser/latency-marker";

async function probe() {
  let video: HTMLVideoElement | null;
  while (!(video = document.querySelector("video"))) await delay(100);
  const source = video;
  const canvas = document.createElement("canvas");
  canvas.width = 640;
  canvas.height = 24;
  const context = canvas.getContext("2d", { willReadFrequently: true })!;
  const frames: { sequence: number; input: number; displayMs: number }[] = [];
  const inputTimes: number[] = [];
  let finished = false;
  const capture: VideoFrameRequestCallback = (_now, metadata) => {
    if (finished) return;
    context.drawImage(source, 0, 0, canvas.width, canvas.height);
    const pixels = context.getImageData(0, 0, canvas.width, canvas.height).data;
    const stride = canvas.width * 4;
    for (const y of [12, 6, 18]) {
      const marker = readMarkerRow(pixels.subarray(stride * y, stride * (y + 1)));
      if (!marker) continue;
      frames.push({ ...marker, displayMs: performance.timeOrigin + metadata.expectedDisplayTime });
      break;
    }
    source.requestVideoFrameCallback(capture);
  };
  source.requestVideoFrameCallback(capture);
  while (frames.length < 30) await delay(50);
  frames.length = 0;
  const start = performance.now();
  const sendKey = (type: string) =>
    source.dispatchEvent(new KeyboardEvent(type, { key: " ", code: "Space", bubbles: true }));
  for (let i = 1; i <= 12; i++) {
    await delay(181 + ((i * 43) % 193));
    inputTimes.push(performance.timeOrigin + performance.now());
    sendKey("keydown");
    await delay(30);
    sendKey("keyup");
    const deadline = performance.now() + 3000;
    while (frames.at(-1)?.input !== i && performance.now() < deadline) await delay(10);
  }
  await delay(Math.max(0, 10_000 - (performance.now() - start)));
  finished = true;
  const result = JSON.stringify({
    frames,
    inputTimes,
    width: source.videoWidth,
    height: source.videoHeight,
  });
  const form = document.createElement("form");
  form.method = "POST";
  form.action = "http://127.0.0.1:7338/results";
  const field = document.createElement("input");
  field.type = "hidden";
  field.name = "report";
  field.value = result;
  form.append(field);
  document.body.append(form);
  form.submit();
}

function delay(ms: number) {
  return new Promise<void>((resolve) => setTimeout(resolve, ms));
}

if (location.hostname === "127.0.0.1" && new URLSearchParams(location.search).has("ios-benchmark"))
  void probe();
