use crate::types::{ConfigFile, EcuDefinition};
use anyhow::{Context, Result, ensure};
use std::{collections::HashSet, fs, path::Path};

pub fn load(path: &Path) -> Result<ConfigFile> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read config {}", path.display()))?;
    let config: ConfigFile = serde_json::from_str(&text)
        .with_context(|| format!("failed to parse config {}", path.display()))?;
    ensure!(
        config.schema_version == 1,
        "unsupported schema_version; expected 1"
    );
    ensure!(!config.ecus.is_empty(), "configuration has no ECUs");
    ensure!(
        !config.supported_bit_rates.is_empty()
            && config
                .supported_bit_rates
                .iter()
                .all(|rate| matches!(rate, 125_000 | 250_000 | 500_000 | 1_000_000)),
        "supported_bit_rates must contain supported CAN rates"
    );
    let mut ids = HashSet::new();
    for (name, ecu) in &config.ecus {
        ensure!(
            !name.is_empty()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
            "invalid ECU name {name}; use lowercase letters, digits, or hyphens"
        );
        ensure!(!ecu.label.trim().is_empty(), "missing label for {name}");
        ensure!(
            config
                .supported_bit_rates
                .contains(&ecu.can.default_bit_rate),
            "unsupported default_bit_rate for {name}"
        );
        ensure!(
            (1..=8).contains(&ecu.can.application_boot_request_data.len()),
            "boot request for {name} must contain 1..8 bytes"
        );
        for id in [
            ecu.can.request_id,
            ecu.can.response_id,
            ecu.can.write_data_id,
            ecu.can.application_boot_request_id,
        ] {
            ensure!(id <= 0x7ff, "CAN ID {id:#x} for {name} must be 11-bit");
            ensure!(ids.insert(id), "duplicate CAN ID {id:#x} for {name}");
        }
    }
    Ok(config)
}

pub fn select_ecu(config: &ConfigFile, name: &str) -> Result<EcuDefinition> {
    config
        .ecus
        .get(&name.to_ascii_lowercase())
        .cloned()
        .with_context(|| {
            format!(
                "ECU '{name}' not found; choose: {}",
                config.ecus.keys().cloned().collect::<Vec<_>>().join(", ")
            )
        })
}
