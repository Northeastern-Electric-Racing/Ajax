use serde::Deserialize;

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
    pub supported_bit_rates: Vec<u32>,
    pub ecus: std::collections::BTreeMap<String, EcuDefinition>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EcuDefinition {
    pub label: String,
    pub can: CanConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanConfig {
    pub default_bit_rate: u32,
    #[serde(deserialize_with = "unsigned")]
    pub request_id: u16,
    #[serde(deserialize_with = "unsigned")]
    pub response_id: u16,
    #[serde(deserialize_with = "unsigned")]
    pub write_data_id: u16,
    #[serde(deserialize_with = "unsigned")]
    pub application_boot_request_id: u16,
    #[serde(deserialize_with = "bytes")]
    pub application_boot_request_data: Vec<u8>,
}

impl EcuDefinition {
    pub fn request_can_id(&self) -> anyhow::Result<u32> {
        Ok(u32::from(self.can.request_id))
    }

    pub fn response_can_id(&self) -> anyhow::Result<u32> {
        Ok(u32::from(self.can.response_id))
    }

    pub fn write_data_can_id(&self) -> anyhow::Result<u32> {
        Ok(u32::from(self.can.write_data_id))
    }

    pub fn boot_request_can_id(&self) -> anyhow::Result<u32> {
        Ok(u32::from(self.can.application_boot_request_id))
    }

    pub fn boot_request_payload(&self) -> anyhow::Result<Vec<u8>> {
        Ok(self.can.application_boot_request_data.clone())
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
