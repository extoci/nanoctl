use std::borrow::Cow;

use anyhow::Result;
use gpui::{AssetSource, SharedString};

pub struct EmbeddedAssets;

impl AssetSource for EmbeddedAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let bytes: Option<&'static [u8]> = match path {
            "icons/monitor.svg" => Some(&include_bytes!("../assets/icons/monitor.svg")[..]),
            "icons/agent.svg" => Some(&include_bytes!("../assets/icons/agent.svg")[..]),
            "icons/labs.svg" => Some(&include_bytes!("../assets/icons/labs.svg")[..]),
            "icons/power.svg" => Some(&include_bytes!("../assets/icons/power.svg")[..]),
            "icons/refresh.svg" => Some(&include_bytes!("../assets/icons/refresh.svg")[..]),
            "icons/folder.svg" => Some(&include_bytes!("../assets/icons/folder.svg")[..]),
            _ => None,
        };
        Ok(bytes.map(Cow::Borrowed))
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}
