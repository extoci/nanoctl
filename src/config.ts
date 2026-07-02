import { Command } from "commander";
import type { CliOptions, RuntimeConfig, SourceMode } from "./types";

function generatePin(): string {
  return String(Math.floor(100000 + Math.random() * 900000));
}

export function parsePositiveInteger(value: string, label: string): number {
  const parsed = Number(value);
  if (!Number.isInteger(parsed) || parsed <= 0) {
    throw new Error(`${label} must be a positive integer.`);
  }
  return parsed;
}

export function parseSourceMode(value: string): SourceMode {
  if (value === "screen" || value === "testsrc") return value;
  throw new Error(`source must be either "screen" or "testsrc", received "${value}".`);
}

function parseCliOptions(): CliOptions {
  const program = new Command();
  program
    .name("nanoctl")
    .allowExcessArguments(false)
    .option("-p, --port <number>", "HTTP server port", (value) =>
      parsePositiveInteger(value, "port"),
    )
    .option("--pin <pin>", "Access PIN for viewers")
    .option("-f, --fps <number>", "Capture and encode frame rate", (value) =>
      parsePositiveInteger(value, "fps"),
    )
    .option("-b, --video-bitrate <bitrate>", "Video bitrate (for example 14M)")
    .option("--use-hwaccel", "Enable hardware encoder on macOS (h264_videotoolbox)")
    .option("--source <mode>", 'Capture source ("screen" or "testsrc")', parseSourceMode)
    .option("--rtp-port <number>", "Local RTP ingress port for video", (value) =>
      parsePositiveInteger(value, "rtp-port"),
    )
    .option("--audio", "Enable system audio streaming")
    .option("--audio-device <value>", "Optional system audio input device override")
    .option("--audio-rtp-port <number>", "Local RTP ingress port for audio", (value) =>
      parsePositiveInteger(value, "audio-rtp-port"),
    )
    .option("--no-control", "Disable remote control input channel")
    .option("--control-bridge-path <path>", "Path to a prebuilt Go control bridge binary");

  program.parse(process.argv);
  return program.opts<CliOptions>();
}

export function getRuntimeConfig(): RuntimeConfig {
  const cli = parseCliOptions();
  const env = process.env;
  const envControlEnabled = env.NANOCTL_NO_CONTROL !== "1";
  const controlEnabled = typeof cli.control === "boolean" ? cli.control : envControlEnabled;

  return {
    port: cli.port ?? parsePositiveInteger(env.NANOCTL_PORT ?? "37777", "NANOCTL_PORT"),
    pin: cli.pin ?? env.NANOCTL_PIN ?? generatePin(),
    fps: cli.fps ?? parsePositiveInteger(env.NANOCTL_FPS ?? "30", "NANOCTL_FPS"),
    videoBitrate: cli.videoBitrate ?? env.NANOCTL_VIDEO_BITRATE ?? "14M",
    useHwaccel: cli.useHwaccel || env.NANOCTL_USE_HWACCEL === "1",
    source: cli.source ?? parseSourceMode(env.NANOCTL_SOURCE ?? "screen"),
    rtpPort:
      cli.rtpPort ?? parsePositiveInteger(env.NANOCTL_RTP_PORT ?? "5004", "NANOCTL_RTP_PORT"),
    audioEnabled: cli.audio || env.NANOCTL_AUDIO === "1",
    audioDevice: cli.audioDevice ?? env.NANOCTL_AUDIO_DEVICE,
    audioRtpPort:
      cli.audioRtpPort ??
      parsePositiveInteger(env.NANOCTL_AUDIO_RTP_PORT ?? "5006", "NANOCTL_AUDIO_RTP_PORT"),
    controlEnabled,
    controlBridgePath: cli.controlBridgePath ?? env.NANOCTL_CONTROL_BRIDGE_PATH,
  };
}
