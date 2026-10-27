use crate::types::{ConfigFile, EcuDefinition, PlatformDefinition};
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
    for (name, platform) in &config.platforms {
        for (region, value) in [("flash", platform.flash), ("ram", platform.ram)] {
            ensure!(
                value.size > 0 && value.address.checked_add(value.size).is_some(),
                "invalid {region} region for platform {name}"
            );
        }
    }
    ensure!(!config.ecus.is_empty(), "configuration has no ECUs");
    ensure!(
        matches!(
            config.can_line_bitrate,
            125_000 | 250_000 | 500_000 | 1_000_000
        ),
        "unsupported can_line_bitrate"
    );
    ensure!(
        (1..=8).contains(&config.application_boot_request_data.len()),
        "application boot request must contain 1..8 bytes"
    );
    let mut ids = HashSet::new();
    for (name, ecu) in &config.ecus {
        ensure!(
            !name.is_empty()
                && name.bytes().all(|b| b.is_ascii_lowercase()
                    || b.is_ascii_digit()
                    || b == b'-'
                    || b == b'_'),
            "invalid ECU name {name}; use lowercase letters, digits, hyphens, or underscores"
        );
        ensure!(!ecu.label.trim().is_empty(), "missing label for {name}");
        ensure!(
            config.platforms.contains_key(&ecu.platform),
            "unknown platform {} for {name}",
            ecu.platform
        );
        for id in [
            ecu.can.request_id,
            ecu.can.response_id,
            ecu.can.write_data_id,
            ecu.can.boot_request_id,
        ] {
            let value = id
                .to_id()
                .with_context(|| format!("invalid CAN ID for {name}"))?;
            ensure!(
                ids.insert(value),
                "duplicate CAN ID {:#x} (is_ext={}) for {name}",
                id.id,
                id.is_ext
            );
        }
    }
    Ok(config)
}

/// Returns the ECU and its platform; `load` guarantees the platform exists.
pub fn select_ecu(config: &ConfigFile, name: &str) -> Result<(EcuDefinition, PlatformDefinition)> {
    let ecu = config
        .ecus
        .get(&name.to_ascii_lowercase())
        .cloned()
        .with_context(|| {
            format!(
                "ECU '{name}' not found; choose: {}",
                config.ecus.keys().cloned().collect::<Vec<_>>().join(", ")
            )
        })?;
    let platform = config.platforms[&ecu.platform];
    Ok((ecu, platform))
}
