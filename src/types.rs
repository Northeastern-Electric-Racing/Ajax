use serde::Deserialize;
use socketcan::{ExtendedId, Id, StandardId};

#[derive(Debug, Clone)]
pub struct Firmware {
    pub data: Vec<u8>,
    pub address: u32,
    pub crc32: u32,
    pub format: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct BootStatus {
    pub version: u8,
}

#[derive(Debug, Deserialize)]
pub struct ConfigFile {
    pub schema_version: u32,
    pub can_line_bitrate: u32,
    #[serde(deserialize_with = "bytes")]
    pub application_boot_request_data: Vec<u8>,
    pub platforms: std::collections::BTreeMap<String, PlatformDefinition>,
    pub ecus: std::collections::BTreeMap<String, EcuDefinition>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformDefinition {
    pub flash: Region,
    pub ram: Region,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Region {
    #[serde(deserialize_with = "unsigned")]
    pub address: u32,
    #[serde(deserialize_with = "unsigned")]
    pub size: u32,
}

impl Region {
    /// Exclusive end address; `config::load` rejects regions that overflow.
    pub fn end(self) -> u32 {
        self.address + self.size
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct EcuDefinition {
    pub label: String,
    pub platform: String,
    pub can: CanConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanConfig {
    pub request_id: CanId,
    pub response_id: CanId,
    pub write_data_id: CanId,
    pub boot_request_id: CanId,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanId {
    #[serde(deserialize_with = "unsigned")]
    pub id: u32,
    pub is_ext: bool,
}

impl CanId {
    pub fn to_id(self) -> anyhow::Result<Id> {
        let id = if self.is_ext {
            ExtendedId::new(self.id).map(Id::Extended)
        } else {
            u16::try_from(self.id)
                .ok()
                .and_then(StandardId::new)
                .map(Id::Standard)
        };
        id.ok_or_else(|| {
            anyhow::anyhow!(
                "CAN ID {:#x} must be {}-bit",
                self.id,
                if self.is_ext { 29 } else { 11 }
            )
        })
    }
}

impl EcuDefinition {
    pub fn request_can_id(&self) -> anyhow::Result<Id> {
        self.can.request_id.to_id()
    }

    pub fn response_can_id(&self) -> anyhow::Result<Id> {
        self.can.response_id.to_id()
    }

    pub fn write_data_can_id(&self) -> anyhow::Result<Id> {
        self.can.write_data_id.to_id()
    }

    pub fn boot_request_can_id(&self) -> anyhow::Result<Id> {
        self.can.boot_request_id.to_id()
    }
}

// Match Atlas: decimal JSON numbers or quoted hexadecimal values with a 0x prefix.
#[derive(Deserialize)]
#[serde(untagged)]
enum Unsigned {
    Decimal(u64),
    Hex(String),
}

impl Unsigned {
    fn value<T: TryFrom<u64>>(self) -> Result<T, String> {
        let value = match self {
            Self::Decimal(value) => value,
            Self::Hex(text) => {
                let digits = text
                    .strip_prefix("0x")
                    .or_else(|| text.strip_prefix("0X"))
                    .ok_or_else(|| format!("Hex value needs a 0x prefix: {text}"))?;
                if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(format!("Invalid hex value: {text}"));
                }
                u64::from_str_radix(digits, 16)
                    .map_err(|_| format!("Hex value overflows: {text}"))?
            }
        };
        T::try_from(value).map_err(|_| format!("Value {value:#x} exceeds field range"))
    }
}

fn unsigned<'de, D: serde::Deserializer<'de>, T: TryFrom<u64>>(
    deserializer: D,
) -> Result<T, D::Error> {
    Unsigned::deserialize(deserializer)?
        .value()
        .map_err(serde::de::Error::custom)
}

fn bytes<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
    Vec::<Unsigned>::deserialize(deserializer)?
        .into_iter()
        .map(|value| value.value().map_err(serde::de::Error::custom))
        .collect()
}
