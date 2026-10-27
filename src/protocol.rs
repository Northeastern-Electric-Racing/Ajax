pub const ACK: u8 = 0x5A;
pub const NACK: u8 = 0xB5;

pub const GET_INFO: u8 = 0xA1;
pub const GET_STATUS: u8 = 0xA2;
pub const START_APP: u8 = 0xA4;
pub const WRITE_MEMORY: u8 = 0xA5;
pub const ACTIVATE: u8 = 0xA7;
pub const COMPUTE_CRC: u8 = 0xA8;
pub const START_UPDATE: u8 = 0xA9;

pub const MAXIMUM_WRITE_SIZE: usize = 256;
pub const WRITE_FRAME_PAYLOAD_SIZE: usize = 7;
