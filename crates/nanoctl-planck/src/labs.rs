use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Windows,
    Macos,
    Linux,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::Macos
        } else {
            Self::Linux
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Windows => "Windows",
            Self::Macos => "macOS",
            Self::Linux => "Linux",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Client,
    Host,
}

pub struct Experiment {
    pub scope: Scope,
    pub id: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub platforms: &'static [Platform],
}

impl Experiment {
    pub fn available_on(&self, platform: Platform) -> bool {
        self.platforms.contains(&platform)
    }

    pub fn available_here(&self) -> bool {
        self.available_on(Platform::current())
    }

    pub fn platform_label(&self) -> String {
        self.platforms
            .iter()
            .map(|platform| platform.label())
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

pub const TAILNET_DISCOVERY: &str = "windows.tailnet-discovery";
pub const STRICT_DXGI_CAPTURE: &str = "windows.strict-dxgi-capture";

pub const EXPERIMENTS: &[Experiment] = &[
    Experiment {
        scope: Scope::Client,
        id: TAILNET_DISCOVERY,
        title: "Tailnet host discovery",
        description: "Probe online Tailscale peers for nanoctl agents and list them beside this PC.",
        platforms: &[Platform::Windows],
    },
    Experiment {
        scope: Scope::Host,
        id: STRICT_DXGI_CAPTURE,
        title: "Strict DXGI capture",
        description: "Require the low-latency DXGI capture path and surface failures instead of falling back to legacy capture.",
        platforms: &[Platform::Windows],
    },
];

pub fn experiment(id: &str) -> Option<&'static Experiment> {
    EXPERIMENTS.iter().find(|experiment| experiment.id == id)
}

#[derive(Clone, Debug, Default)]
pub struct Labs {
    values: BTreeMap<String, bool>,
}

impl Labs {
    pub fn new(values: BTreeMap<String, bool>) -> Self {
        Self { values }
    }

    pub fn active(&self, id: &str) -> bool {
        experiment(id).is_some_and(|experiment| experiment.available_here())
            && self.values.get(id).copied().unwrap_or(false)
    }

    pub fn stored(&self, id: &str) -> bool {
        self.values.get(id).copied().unwrap_or(false)
    }

    pub fn set(&mut self, id: &str, enabled: bool) -> bool {
        let Some(experiment) = experiment(id) else {
            return false;
        };
        if enabled && !experiment.available_here() {
            return false;
        }
        self.values.insert(id.to_owned(), enabled);
        true
    }

    pub fn values(&self) -> &BTreeMap<String, bool> {
        &self.values
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn experiments_default_off() {
        let labs = Labs::default();
        assert!(!labs.active(TAILNET_DISCOVERY));
        assert!(!labs.stored(TAILNET_DISCOVERY));
    }

    #[test]
    fn windows_tailnet_discovery_is_windows_only() {
        let experiment = experiment(TAILNET_DISCOVERY).unwrap();
        assert!(experiment.available_on(Platform::Windows));
        assert!(!experiment.available_on(Platform::Macos));
        assert!(!experiment.available_on(Platform::Linux));
        assert_eq!(experiment.platform_label(), "Windows");
    }

    #[test]
    fn unavailable_experiments_cannot_activate() {
        let mut labs = Labs::default();
        let changed = labs.set(TAILNET_DISCOVERY, true);
        assert_eq!(changed, cfg!(target_os = "windows"));
        assert_eq!(labs.active(TAILNET_DISCOVERY), cfg!(target_os = "windows"));
        assert!(!labs.set("missing", true));
    }

    #[test]
    fn unknown_values_are_preserved_for_newer_clients() {
        let mut values = BTreeMap::new();
        values.insert("future.feature".to_owned(), true);
        let labs = Labs::new(values);
        assert!(labs.values()["future.feature"]);
    }
}
