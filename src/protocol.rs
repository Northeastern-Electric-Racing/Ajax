pub const ACK: u8 = 0x5A;
pub const NACK: u8 = 0xA5;

pub const GET: u8 = 0xA1;
pub const GET_VERSION: u8 = 0xA2;
pub const GO: u8 = 0xA4;
pub const WRITE_MEMORY: u8 = 0xA5;
pub const ERASE_MEMORY: u8 = 0xA6;
pub const BANK_SWAP: u8 = 0xA7;
pub const COMPUTE_CRC: u8 = 0xA8;
pub const START_UPDATE: u8 = 0xA9;
pub const SET_BAUD_RATE: u8 = 0xAA;

pub const MAXIMUM_WRITE_SIZE: usize = 256;
pub const WRITE_FRAME_PAYLOAD_SIZE: usize = 7;

pub const LOGICAL_APPLICATION_ADDRESS: u32 = 0x0800_8000;
pub const BANK_SIZE: u32 = 0x0010_0000;
pub const INACTIVE_APPLICATION_ADDRESS: u32 = LOGICAL_APPLICATION_ADDRESS + BANK_SIZE;
pub const MAXIMUM_IMAGE_SIZE: usize = 0x000F_8000;

pub const FIRST_APPLICATION_SECTOR: u8 = 4;
pub const APPLICATION_SECTOR_COUNT: u8 = 124;

pub fn baud_rate_code(bit_rate: u32) -> anyhow::Result<u8> {
    match bit_rate {
        125_000 => Ok(0x00),
        250_000 => Ok(0x01),
        500_000 => Ok(0x02),
        1_000_000 => Ok(0x03),
        _ => anyhow::bail!("unsupported CAN baud rate: {bit_rate}"),
    }
}
