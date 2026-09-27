pub const ACK: u8 = 0x5A;
pub const NACK: u8 = 0xB5;

pub const GET_INFO: u8 = 0xA1;
pub const GET_STATUS: u8 = 0xA2;
pub const START_APP: u8 = 0xA4;
pub const WRITE_MEMORY: u8 = 0xA5;
pub const ACTIVATE: u8 = 0xA7;
pub const COMPUTE_CRC: u8 = 0xA8;
pub const START_UPDATE: u8 = 0xA9;
pub const SET_BAUD_RATE: u8 = 0xAA;

pub const MAXIMUM_WRITE_SIZE: usize = 256;
pub const WRITE_FRAME_PAYLOAD_SIZE: usize = 7;

pub const LOGICAL_APPLICATION_ADDRESS: u32 = 0x0802_E000;
pub const MAXIMUM_IMAGE_SIZE: usize = 0x000D_2000;

pub fn baud_rate_code(bit_rate: u32) -> anyhow::Result<u8> {
    match bit_rate {
        125_000 => Ok(0x00),
        250_000 => Ok(0x01),
        500_000 => Ok(0x02),
        1_000_000 => Ok(0x03),
        _ => anyhow::bail!("unsupported CAN baud rate: {bit_rate}"),
    }
}
