# nanoctl v0

`nanoctl` is `nanoshare`, but with a minimal remote-control path on top.

v0 is intentionally small:

- bun + typescript app for signaling, auth, pages, and capture management
- webrtc screen/audio stream to the remote viewer
- a webrtc data channel for control events
- a tiny go helper that injects host input events

## status

this is a demo-focused v0. it aims to prove the full flow, not solve every platform or security edge case yet.

current control support:

- linux/x11 host: supported through X11/XTEST
- macos/windows host: stream still works, control channel is disabled for now

## requirements

- [bun](https://bun.com)
- [go](https://go.dev)
- `ffmpeg` in `PATH`
- linux/x11 hosts need an active `DISPLAY` with the XTEST extension available
- host and viewer on the same network

## run

install deps:

```bash
bun install
```

start the host:

```bash
bun run src/index.ts
```

or with a test pattern:

```bash
bun run src/index.ts --source testsrc
```

open the printed LAN URL on the other machine, enter the PIN, and the viewer page will connect automatically.

## host controls

- `Ctrl+C` stops the host
- press `a` in the host terminal to toggle audio on and off

## viewer controls

when control is enabled on the host:

- click the video to focus it
- move the mouse to move the host pointer
- click to click on the host
- type for basic key presses
- press `f` for fullscreen

## flags

- `--port <number>`
- `--pin <pin>`
- `--fps <number>`
- `--video-bitrate <bitrate>`
- `--use-hwaccel`
- `--source <screen|testsrc>`
- `--rtp-port <number>`
- `--audio`
- `--audio-device <value>`
- `--audio-rtp-port <number>`
- `--no-control`
- `--control-bridge-path <path>`

## env vars

- `NANOCTL_PORT`
- `NANOCTL_PIN`
- `NANOCTL_FPS`
- `NANOCTL_VIDEO_BITRATE`
- `NANOCTL_USE_HWACCEL=1`
- `NANOCTL_SOURCE=screen|testsrc`
- `NANOCTL_RTP_PORT`
- `NANOCTL_AUDIO=1`
- `NANOCTL_AUDIO_DEVICE`
- `NANOCTL_AUDIO_RTP_PORT`
- `NANOCTL_DISPLAY`
- `NANOCTL_CONTROL_BRIDGE_PATH`

## notes

the control helper is built on first run if you do not provide `--control-bridge-path`.
