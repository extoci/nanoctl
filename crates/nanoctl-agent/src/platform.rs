use serde::Serialize;

use crate::config::{AgentConfig, EncoderPreference};

#[cfg(target_os = "windows")]
pub const PLATFORM: &str = "windows";
#[cfg(target_os = "macos")]
pub const PLATFORM: &str = "macos";
#[cfg(target_os = "linux")]
pub const PLATFORM: &str = "linux";
#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
compile_error!("nanoctl supports only Windows, macOS, and Linux");

#[cfg(target_arch = "x86_64")]
pub const ARCHITECTURE: &str = "x64";
#[cfg(target_arch = "aarch64")]
pub const ARCHITECTURE: &str = "arm64";
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
compile_error!("nanoctl supports only x64 and arm64");

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    protocol_version: u8,
    platform: &'static str,
    architecture: &'static str,
    codecs: Vec<&'static str>,
    encoder_backends: Vec<&'static str>,
    ready: bool,
    readiness: &'static str,
    input: bool,
    remote_input: bool,
    gamepad: bool,
    clipboard: bool,
    system_audio: bool,
    #[cfg(all(feature = "media", target_os = "windows"))]
    #[serde(skip_serializing_if = "Option::is_none")]
    audio_outputs: Option<Vec<crate::audio::AudioOutput>>,
    displays: Vec<DisplayCapability>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DisplayCapability {
    id: String,
    name: String,
    width: u32,
    height: u32,
    scale_factor: f32,
    primary: bool,
}

#[derive(Debug, Serialize)]
pub struct DoctorReport {
    pub ready: bool,
    pub checks: Vec<DoctorCheck>,
}

#[derive(Debug, Serialize)]
pub struct DoctorCheck {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
}

pub fn capabilities(
    remote_input_enabled: bool,
    clipboard_enabled: bool,
    system_audio_enabled: bool,
    encoder_preference: EncoderPreference,
) -> Capabilities {
    let displays = display_capabilities();
    let input = input_available();
    let encoder_backends = {
        #[cfg(feature = "media")]
        {
            match crate::media::probe_encoder(encoder_preference) {
                Ok((id, _))
                    if id != "software-openh264"
                        && matches!(encoder_preference, EncoderPreference::Auto) =>
                {
                    vec![id, "software-openh264"]
                }
                Ok((id, _)) => vec![id],
                Err(_) => Vec::new(),
            }
        }
        #[cfg(not(feature = "media"))]
        {
            let _ = encoder_preference;
            Vec::new()
        }
    };
    let encoder_ready = !encoder_backends.is_empty();
    let readiness = if !cfg!(feature = "media") {
        "encoder-unavailable"
    } else if displays.is_empty() {
        "no-display"
    } else if !encoder_ready {
        "encoder-unavailable"
    } else if remote_input_enabled && !input {
        "input-permission"
    } else {
        "ready"
    };
    #[cfg(all(feature = "media", target_os = "windows"))]
    let audio_outputs = audio_outputs(system_audio_enabled);
    Capabilities {
        protocol_version: 2,
        platform: PLATFORM,
        architecture: ARCHITECTURE,
        codecs: {
            #[cfg(feature = "media")]
            {
                crate::media::ensure_codecs_probed(encoder_preference);
                crate::codec::host_codecs()
                    .into_iter()
                    .map(crate::codec::VideoCodec::id)
                    .collect()
            }
            #[cfg(not(feature = "media"))]
            {
                Vec::new()
            }
        },
        encoder_backends,
        ready: cfg!(feature = "media")
            && !displays.is_empty()
            && encoder_ready
            && (!remote_input_enabled || input),
        readiness,
        input: remote_input_enabled && input,
        remote_input: remote_input_enabled && input,
        gamepad: gamepad_available(remote_input_enabled && input),
        clipboard: clipboard_available(clipboard_enabled && remote_input_enabled && input),
        #[cfg(all(feature = "media", target_os = "windows"))]
        system_audio: audio_outputs
            .as_ref()
            .is_some_and(|outputs| !outputs.is_empty()),
        #[cfg(all(feature = "media", target_os = "windows"))]
        audio_outputs,
        #[cfg(not(all(feature = "media", target_os = "windows")))]
        system_audio: system_audio_available(system_audio_enabled),
        displays,
    }
}

fn gamepad_available(enabled: bool) -> bool {
    #[cfg(feature = "media")]
    {
        enabled && crate::gamepad::available()
    }
    #[cfg(not(feature = "media"))]
    {
        let _ = enabled;
        false
    }
}

fn clipboard_available(enabled: bool) -> bool {
    #[cfg(feature = "media")]
    {
        enabled && crate::clipboard::backend().is_some()
    }
    #[cfg(not(feature = "media"))]
    {
        let _ = enabled;
        false
    }
}

#[cfg(all(feature = "media", target_os = "windows"))]
fn audio_outputs(enabled: bool) -> Option<Vec<crate::audio::AudioOutput>> {
    enabled.then(crate::audio::outputs)
}

#[cfg(not(all(feature = "media", target_os = "windows")))]
fn system_audio_available(enabled: bool) -> bool {
    #[cfg(feature = "media")]
    {
        enabled && crate::audio::available()
    }
    #[cfg(not(feature = "media"))]
    {
        let _ = enabled;
        false
    }
}

fn input_available() -> bool {
    #[cfg(feature = "media")]
    {
        crate::input::probe().is_ok()
    }
    #[cfg(not(feature = "media"))]
    {
        false
    }
}

fn display_capabilities() -> Vec<DisplayCapability> {
    #[cfg(feature = "media")]
    {
        xcap::Monitor::all()
            .unwrap_or_default()
            .into_iter()
            .take(16)
            .filter_map(|monitor| {
                Some(DisplayCapability {
                    id: monitor.id().ok()?.to_string(),
                    name: monitor
                        .friendly_name()
                        .or_else(|_| monitor.name())
                        .unwrap_or_else(|_| "Display".to_owned()),
                    width: monitor.width().ok()?,
                    height: monitor.height().ok()?,
                    scale_factor: monitor.scale_factor().unwrap_or(1.0),
                    primary: monitor.is_primary().unwrap_or(false),
                })
            })
            .collect()
    }
    #[cfg(not(feature = "media"))]
    {
        Vec::new()
    }
}

pub async fn doctor(config: &AgentConfig) -> DoctorReport {
    let mut checks = Vec::new();
    checks.push(DoctorCheck {
        name: "configuration",
        ok: config.validate().is_ok(),
        detail: if config.validate().is_ok() {
            "valid".into()
        } else {
            "invalid; run nanoctl config and inspect service logs".into()
        },
    });
    let found = crate::tailnet::CliTailnet::discover(&config.tailnet);
    checks.push(DoctorCheck {
        name: "tailscale CLI",
        ok: found.is_ok(),
        detail: match &found {
            Ok(_) => "found".into(),
            Err(error) => error.to_string(),
        },
    });
    let status = crate::tailnet::real(&config.tailnet).status().await;
    if let Ok(node) = &status {
        let tls = crate::tls::inspect(&config.tailnet, node);
        checks.push(DoctorCheck {
            name: "TLS",
            ok: true,
            detail: tls.detail,
        });
    }
    checks.push(DoctorCheck {
        name: "tailnet",
        ok: status.is_ok(),
        detail: match status {
            Ok(node) => format!(
                "running; IPs={:?}; MagicDNS={}; open {}",
                node.ips,
                node.dns_name,
                crate::tls::viewer_url(
                    &node,
                    config.tailnet.port,
                    crate::tls::inspect(&config.tailnet, &node).config.is_some()
                )
                .unwrap_or_default()
            ),
            Err(error) => error.to_string(),
        },
    });
    checks.push(capture_check());
    checks.push(encoder_check(config.quality.encoder));
    checks.push(input_check());
    DoctorReport {
        ready: checks.iter().all(|check| check.ok),
        checks,
    }
}

impl DoctorReport {
    pub fn print(&self) {
        for check in &self.checks {
            println!(
                "{:>4}  {:<16} {}",
                if check.ok { "ok" } else { "fail" },
                check.name,
                check.detail
            );
        }
        println!(
            "\n{}",
            if self.ready {
                "Agent is ready."
            } else {
                "Agent is not ready."
            }
        );
    }
}

fn capture_check() -> DoctorCheck {
    #[cfg(feature = "media")]
    {
        let result = xcap::Monitor::all();
        DoctorCheck {
            name: "capture",
            ok: result.as_ref().is_ok_and(|monitors| !monitors.is_empty()),
            detail: match result {
                Ok(monitors) if !monitors.is_empty() => {
                    format!("{} display(s) available", monitors.len())
                }
                Ok(_) => "no displays available in the interactive session".into(),
                Err(error) => format!("{error}"),
            },
        }
    }
    #[cfg(not(feature = "media"))]
    DoctorCheck {
        name: "capture",
        ok: false,
        detail: format!(
            "media backend not compiled; build with --features media ({})",
            platform_capture_guidance()
        ),
    }
}

fn input_check() -> DoctorCheck {
    #[cfg(feature = "media")]
    {
        let result = crate::input::probe();
        DoctorCheck {
            name: "input",
            ok: result.is_ok(),
            detail: match result {
                Ok(()) => platform_input_guidance().into(),
                Err(error) => format!("{error:#}"),
            },
        }
    }
    #[cfg(not(feature = "media"))]
    DoctorCheck {
        name: "input",
        ok: false,
        detail: "media/input backend not compiled; build with --features media".into(),
    }
}

fn encoder_check(_preference: crate::config::EncoderPreference) -> DoctorCheck {
    #[cfg(feature = "media")]
    {
        let result = crate::media::probe_encoder(_preference);
        DoctorCheck {
            name: "encoder",
            ok: result.is_ok(),
            detail: match result {
                Ok((_, backend)) => format!("{backend} real-time H.264 encoder available"),
                Err(error) => format!("{error:#}"),
            },
        }
    }
    #[cfg(not(feature = "media"))]
    DoctorCheck {
        name: "encoder",
        ok: false,
        detail: "H.264 encoder not compiled; build with --features media".into(),
    }
}

#[cfg(target_os = "linux")]
#[cfg(not(feature = "media"))]
fn platform_capture_guidance() -> &'static str {
    "backend requires PipeWire portal or X11 session probe"
}
#[cfg(target_os = "windows")]
#[cfg(not(feature = "media"))]
fn platform_capture_guidance() -> &'static str {
    "backend requires interactive Windows.Graphics.Capture probe"
}
#[cfg(target_os = "macos")]
#[cfg(not(feature = "media"))]
fn platform_capture_guidance() -> &'static str {
    "grant Screen Recording permission, then rerun doctor"
}

#[cfg(target_os = "linux")]
#[cfg(feature = "media")]
fn platform_input_guidance() -> &'static str {
    "backend requires RemoteDesktop portal or XTest probe"
}
#[cfg(target_os = "windows")]
#[cfg(feature = "media")]
fn platform_input_guidance() -> &'static str {
    "backend requires interactive SendInput probe"
}
#[cfg(target_os = "macos")]
#[cfg(feature = "media")]
fn platform_input_guidance() -> &'static str {
    "grant Accessibility permission, then rerun doctor"
}

#[cfg(test)]
mod audio_tests {
    #[test]
    fn disabled_audio_is_never_advertised() {
        #[cfg(all(feature = "media", target_os = "windows"))]
        assert!(super::audio_outputs(false).is_none());
        #[cfg(not(all(feature = "media", target_os = "windows")))]
        assert!(!super::system_audio_available(false));
        #[cfg(any(not(feature = "media"), target_os = "macos"))]
        assert!(!super::system_audio_available(true));
    }
}
