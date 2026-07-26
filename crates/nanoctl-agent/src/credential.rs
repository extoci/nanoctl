use anyhow::{Context, Result};
use keyring::Entry;
use zeroize::Zeroizing;

const SERVICE: &str = "dev.nanoctl.agent";

pub fn store(device_id: &str, token: &str) -> Result<()> {
    Entry::new(SERVICE, device_id)
        .context("credential store unavailable")?
        .set_password(token)
        .context("could not store device credential")
}

pub fn load(device_id: &str) -> Result<Zeroizing<String>> {
    let value = Entry::new(SERVICE, device_id)
        .context("credential store unavailable")?
        .get_password()
        .context("device credential missing; re-enroll this device")?;
    Ok(Zeroizing::new(value))
}
