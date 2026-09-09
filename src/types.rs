use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct Firmware {
    pub data: Vec<u8>,
    pub address: u32,
    pub crc32: u32,
    pub format: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct BootInfo {
    pub version: u8,
    pub bank_swap_enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct ConfigFile {
    #[serde(rename = "supportedBitRates")]
    pub supported_bit_rates: Vec<u32>,
    pub ecus: Vec<EcuDefinition>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EcuDefinition {
    pub name: String,
    pub label: String,
    #[serde(rename = "requestId")]
    pub request_id: String,
    #[serde(rename = "responseId")]
    pub response_id: String,
    #[serde(rename = "writeDataId")]
    pub write_data_id: String,
    #[serde(rename = "applicationBootRequestId")]
    pub application_boot_request_id: String,
    #[serde(rename = "applicationBootRequestData")]
    pub application_boot_request_data: String,
}

impl EcuDefinition {
    pub fn request_can_id(&self) -> anyhow::Result<u32> {
        parse_u32(&self.request_id)
    }

    pub fn response_can_id(&self) -> anyhow::Result<u32> {
        parse_u32(&self.response_id)
    }

    pub fn write_data_can_id(&self) -> anyhow::Result<u32> {
        parse_u32(&self.write_data_id)
    }

    pub fn boot_request_can_id(&self) -> anyhow::Result<u32> {
        parse_u32(&self.application_boot_request_id)
    }

    pub fn boot_request_payload(&self) -> anyhow::Result<Vec<u8>> {
        let mut out = Vec::new();
        for token in self.application_boot_request_data.split_whitespace() {
            let value = u8::from_str_radix(token.trim_start_matches("0x"), 16)?;
            out.push(value);
        }
        anyhow::ensure!(!out.is_empty() && out.len() <= 8, "boot request must contain 1..8 bytes");
        Ok(out)
    }
}

fn parse_u32(text: &str) -> anyhow::Result<u32> {
    let text = text.trim();
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Ok(u32::from_str_radix(hex, 16)?)
    } else {
        Ok(text.parse::<u32>()?)
    }
}
