use serde::Serialize;

use crate::config::AgentConfig;

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
    input: bool,
    clipboard: bool,
    system_audio: bool,
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

pub fn capabilities() -> Capabilities {
    Capabilities {
        protocol_version: 1,
        platform: PLATFORM,
        architecture: ARCHITECTURE,
        codecs: if cfg!(feature = "media") {
            vec!["h264"]
        } else {
            Vec::new()
        },
        input: cfg!(feature = "media"),
        clipboard: false,
        system_audio: false,
        displays: display_capabilities(),
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

pub fn default_device_name() -> String {
    hostname::get()
        .ok()
        .and_then(|value| value.into_string().ok())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("{} computer", PLATFORM))
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
    let credential_ok = config
        .device_id
        .as_deref()
        .is_some_and(|id| crate::credential::load(id).is_ok());
    checks.push(DoctorCheck {
        name: "credential",
        ok: credential_ok,
        detail: if credential_ok {
            "available in operating-system credential store".into()
        } else {
            "missing; enroll this device".into()
        },
    });
    checks.push(capture_check());
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
        DoctorCheck {
            name: "input",
            ok: true,
            detail: platform_input_guidance().into(),
        }
    }
    #[cfg(not(feature = "media"))]
    DoctorCheck {
        name: "input",
        ok: false,
        detail: "media/input backend not compiled; build with --features media".into(),
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
