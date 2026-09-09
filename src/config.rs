use crate::types::{ConfigFile, EcuDefinition};
use anyhow::{Context, Result};
use std::{fs, path::Path};

pub fn load(path: &Path) -> Result<ConfigFile> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read config {}", path.display()))?;
    Ok(serde_json::from_str(&text)
        .with_context(|| format!("failed to parse config {}", path.display()))?)
}

pub fn select_ecu(config: &ConfigFile, name: &str) -> Result<EcuDefinition> {
    config
        .ecus
        .iter()
        .find(|ecu| ecu.name.eq_ignore_ascii_case(name))
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("ECU '{name}' not found in config"))
}
