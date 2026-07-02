import ffmpegStatic from "ffmpeg-static";

export function resolveFfmpegBinary(): string {
  if (process.env.FFMPEG_PATH?.trim()) {
    return process.env.FFMPEG_PATH.trim();
  }

  if (ffmpegStatic) {
    return ffmpegStatic;
  }

  return "ffmpeg";
}
