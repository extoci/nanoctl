import { spawnSync } from "node:child_process";
import { platform } from "node:os";
import { resolveFfmpegBinary } from "./ffmpeg";
import type { AudioCaptureConfig, RuntimeConfig, VideoCaptureConfig } from "./types";

const VIDEO_INPUT_QUEUE_SIZE = "64";
const AUDIO_INPUT_QUEUE_SIZE = "64";
const FFMPEG_BINARY = resolveFfmpegBinary();

function detectMacScreenInputIndex(): number {
  const probe = spawnSync(
    FFMPEG_BINARY,
    ["-hide_banner", "-f", "avfoundation", "-list_devices", "true", "-i", ""],
    {
      encoding: "utf8",
    },
  );
  const output = `${probe.stdout ?? ""}\n${probe.stderr ?? ""}`;
  const regex = /\[(\d+)\]\s+Capture screen/i;
  const match = output.match(regex);
  if (!match) {
    throw new Error(
      "Could not auto-detect a macOS capture device. Ensure Screen Recording permissions are granted.",
    );
  }
  return Number(match[1]);
}

function listMacAudioDevices(): Array<{ index: number; name: string }> {
  const probe = spawnSync(
    FFMPEG_BINARY,
    ["-hide_banner", "-f", "avfoundation", "-list_devices", "true", "-i", ""],
    {
      encoding: "utf8",
    },
  );
  const output = `${probe.stdout ?? ""}\n${probe.stderr ?? ""}`;
  const lines = output.split(/\r?\n/);
  const devices: Array<{ index: number; name: string }> = [];
  let inAudioSection = false;

  for (const rawLine of lines) {
    const line = rawLine.trim();
    if (line.includes("AVFoundation audio devices")) {
      inAudioSection = true;
      continue;
    }
    if (line.includes("AVFoundation video devices")) {
      inAudioSection = false;
      continue;
    }
    if (!inAudioSection) continue;
    const match = line.match(/\[(\d+)\]\s+(.+)$/);
    if (!match) continue;
    devices.push({ index: Number(match[1]), name: match[2] });
  }

  return devices;
}

function detectWindowsPrimaryBounds(): {
  x: number;
  y: number;
  width: number;
  height: number;
} | null {
  const probe = spawnSync(
    "powershell",
    [
      "-NoProfile",
      "-Command",
      'Add-Type -AssemblyName System.Windows.Forms; $b=[System.Windows.Forms.Screen]::PrimaryScreen.Bounds; Write-Output "$($b.X),$($b.Y),$($b.Width),$($b.Height)"',
    ],
    { encoding: "utf8" },
  );

  if (probe.status !== 0) return null;
  const output = `${probe.stdout ?? ""}`.trim();
  const match = output.match(/^(-?\d+),(-?\d+),(\d+),(\d+)$/);
  if (!match) return null;

  return {
    x: Number(match[1]),
    y: Number(match[2]),
    width: Number(match[3]),
    height: Number(match[4]),
  };
}

export function buildVideoCaptureConfig(config: RuntimeConfig): VideoCaptureConfig {
  if (config.source === "testsrc") {
    return {
      source: `FFmpeg lavfi testsrc (${config.fps}fps)`,
      ffmpegInputArgs: [
        "-re",
        "-thread_queue_size",
        VIDEO_INPUT_QUEUE_SIZE,
        "-f",
        "lavfi",
        "-i",
        `testsrc2=size=1920x1080:rate=${config.fps}`,
      ],
    };
  }

  const currentPlatform = platform();

  if (currentPlatform === "darwin") {
    const screenIndex = detectMacScreenInputIndex();
    return {
      source: `macOS avfoundation screen index ${screenIndex}`,
      ffmpegInputArgs: [
        "-thread_queue_size",
        VIDEO_INPUT_QUEUE_SIZE,
        "-f",
        "avfoundation",
        "-framerate",
        String(config.fps),
        "-capture_cursor",
        "1",
        "-capture_mouse_clicks",
        "1",
        "-i",
        `${screenIndex}:none`,
      ],
    };
  }

  if (currentPlatform === "win32") {
    const bounds = detectWindowsPrimaryBounds();
    return {
      source: bounds
        ? `Windows gdigrab primary screen ${bounds.width}x${bounds.height} @ (${bounds.x},${bounds.y})`
        : "Windows gdigrab desktop",
      ffmpegInputArgs: [
        "-thread_queue_size",
        VIDEO_INPUT_QUEUE_SIZE,
        "-f",
        "gdigrab",
        "-framerate",
        String(config.fps),
        "-draw_mouse",
        "1",
        ...(bounds
          ? [
              "-offset_x",
              String(bounds.x),
              "-offset_y",
              String(bounds.y),
              "-video_size",
              `${bounds.width}x${bounds.height}`,
            ]
          : []),
        "-i",
        "desktop",
      ],
    };
  }

  if (currentPlatform === "linux") {
    const display = process.env.NANOCTL_DISPLAY ?? process.env.DISPLAY ?? ":0.0";
    return {
      source: `Linux x11grab ${display}`,
      ffmpegInputArgs: [
        "-thread_queue_size",
        VIDEO_INPUT_QUEUE_SIZE,
        "-f",
        "x11grab",
        "-framerate",
        String(config.fps),
        "-i",
        display,
      ],
    };
  }

  throw new Error(`Unsupported platform: ${currentPlatform}`);
}

export function buildAudioCaptureConfig(config: RuntimeConfig): AudioCaptureConfig {
  const currentPlatform = platform();
  const audioDevice = config.audioDevice?.trim();

  if (currentPlatform === "darwin") {
    if (audioDevice) {
      return {
        source: `macOS avfoundation audio device ${audioDevice}`,
        ffmpegInputArgs: [
          "-thread_queue_size",
          AUDIO_INPUT_QUEUE_SIZE,
          "-f",
          "avfoundation",
          "-i",
          `:${audioDevice}`,
        ],
      };
    }

    const devices = listMacAudioDevices();
    const preferred = devices.find((device) =>
      /blackhole|loopback|soundflower|virtual/i.test(device.name),
    );
    if (!preferred) {
      throw new Error(
        "No macOS loopback audio device detected. Install/configure BlackHole/Loopback/Soundflower or pass --audio-device.",
      );
    }

    return {
      source: `macOS avfoundation ${preferred.name} (#${preferred.index})`,
      ffmpegInputArgs: [
        "-thread_queue_size",
        AUDIO_INPUT_QUEUE_SIZE,
        "-f",
        "avfoundation",
        "-i",
        `:${preferred.index}`,
      ],
    };
  }

  if (currentPlatform === "win32") {
    const device = audioDevice || "default";
    return {
      source: `Windows WASAPI ${device}`,
      ffmpegInputArgs: [
        "-f",
        "wasapi",
        "-thread_queue_size",
        AUDIO_INPUT_QUEUE_SIZE,
        "-loopback",
        "1",
        "-i",
        device,
      ],
    };
  }

  if (currentPlatform === "linux") {
    const device = audioDevice || "default";
    return {
      source: `Linux PulseAudio ${device}`,
      ffmpegInputArgs: ["-f", "pulse", "-thread_queue_size", AUDIO_INPUT_QUEUE_SIZE, "-i", device],
    };
  }

  throw new Error(`Unsupported platform for audio capture: ${currentPlatform}`);
}

export function buildCodecArgs(config: RuntimeConfig): string[] {
  if (platform() === "darwin" && config.useHwaccel) {
    return [
      "-vf",
      "scale=trunc(iw/2)*2:trunc(ih/2)*2",
      "-c:v",
      "h264_videotoolbox",
      "-realtime",
      "1",
      "-allow_sw",
      "1",
      "-b:v",
      config.videoBitrate,
      "-maxrate",
      config.videoBitrate,
      "-bufsize",
      "6M",
      "-g",
      String(config.fps),
      "-profile:v",
      "baseline",
      "-level",
      "3.1",
      "-pix_fmt",
      "yuv420p",
    ];
  }

  return [
    "-vf",
    "scale=trunc(iw/2)*2:trunc(ih/2)*2",
    "-c:v",
    "libx264",
    "-preset",
    "veryfast",
    "-tune",
    "zerolatency",
    "-pix_fmt",
    "yuv420p",
    "-b:v",
    config.videoBitrate,
    "-maxrate",
    config.videoBitrate,
    "-bufsize",
    "6M",
    "-g",
    String(config.fps),
    "-profile:v",
    "baseline",
    "-level",
    "3.1",
  ];
}
