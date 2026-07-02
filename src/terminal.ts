import { networkInterfaces } from "node:os";
import * as readline from "node:readline";

const ANSI = {
  reset: "\x1b[0m",
  bold: "\x1b[1m",
  dim: "\x1b[2m",
  cyan: "\x1b[36m",
  green: "\x1b[32m",
  yellow: "\x1b[33m",
} as const;

export function paint(text: string, ...codes: string[]): string {
  return `${codes.join("")}${text}${ANSI.reset}`;
}

export function getLanIp(): string {
  const interfaces = networkInterfaces();
  for (const entries of Object.values(interfaces)) {
    if (!entries) continue;
    for (const entry of entries) {
      if (entry.family === "IPv4" && !entry.internal) return entry.address;
    }
  }
  return "127.0.0.1";
}

type CliPanelOptions = {
  lanUrl: string;
  localUrl: string;
  pin: string;
  fps: number;
  videoBitrate: string;
  audioText: string;
  controlText: string;
};

export function printCliPanel({
  lanUrl,
  localUrl,
  pin,
  fps,
  videoBitrate,
  audioText,
  controlText,
}: CliPanelOptions): void {
  const toggleHint = process.stdin.isTTY ? " | Toggle Audio: A" : "";
  const rows = [
    { text: "Nanoctl v0", tone: "title" as const },
    { text: "Open URL on same network, enter PIN once.", tone: "hint" as const },
    { text: `LAN URL : ${lanUrl}`, tone: "normal" as const },
    { text: `Local   : ${localUrl}`, tone: "normal" as const },
    { text: `PIN     : ${pin}`, tone: "pin" as const },
    { text: `Video   : ${fps} fps | ${videoBitrate} | WebRTC H.264`, tone: "normal" as const },
    { text: `Audio   : ${audioText}`, tone: "normal" as const },
    { text: `Control : ${controlText}`, tone: "normal" as const },
    { text: `Stop    : Ctrl+C${toggleHint}`, tone: "hint" as const },
  ];

  const width = rows.reduce((max, row) => Math.max(max, row.text.length), 0);
  const top = `┌${"─".repeat(width + 2)}┐`;
  const bottom = `└${"─".repeat(width + 2)}┘`;

  console.log("");
  console.log(paint(top, ANSI.cyan));
  for (const row of rows) {
    const line = `│ ${row.text.padEnd(width)} │`;
    if (row.tone === "title") {
      console.log(paint(line, ANSI.bold, ANSI.green));
      continue;
    }
    if (row.tone === "pin") {
      console.log(paint(line, ANSI.bold, ANSI.yellow));
      continue;
    }
    if (row.tone === "hint") {
      console.log(paint(line, ANSI.dim));
      continue;
    }
    console.log(line);
  }
  console.log(paint(bottom, ANSI.cyan));
  console.log("");
}

type KeypressHandler = (
  input: string,
  key: { name?: string; ctrl?: boolean; meta?: boolean; sequence?: string },
) => void;

export function setupRuntimeKeyControls(
  onToggleAudio: () => void,
  shutdown: () => void,
): () => void {
  const stdin = process.stdin;
  if (!stdin.isTTY) return () => {};

  readline.emitKeypressEvents(stdin);
  stdin.setRawMode(true);
  stdin.resume();

  const onKeypress: KeypressHandler = (_input, key) => {
    if (!key) return;
    if (key.ctrl && key.name === "c") {
      shutdown();
      return;
    }
    if (key.meta || key.ctrl || key.name !== "a") return;
    onToggleAudio();
  };

  stdin.on("keypress", onKeypress);

  return () => {
    stdin.off("keypress", onKeypress);
    stdin.setRawMode(false);
  };
}
