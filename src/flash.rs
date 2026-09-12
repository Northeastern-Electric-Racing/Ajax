use crate::{protocol, can::{CanFrame, SocketCan}, types::{BootInfo, EcuDefinition, Firmware}};
use anyhow::{Context, Result};
use std::{io::{self, Write}, thread, time::{Duration, Instant}};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const PACKET_TIMEOUT: Duration = Duration::from_millis(500);
const ERASE_TIMEOUT: Duration = Duration::from_secs(30);
const RETRY_DELAY: Duration = Duration::from_millis(100);
const BOOT_DELAY: Duration = Duration::from_millis(750);
const MAX_ATTEMPTS: usize = 3;
const MAX_WRITE_PACKET_ATTEMPTS: usize = 4;

enum AckStatus {
    Ack,
    Nack,
    Timeout,
}

pub struct FirmwareFlashManager {
    can: SocketCan,
    ecu: EcuDefinition,
}

impl FirmwareFlashManager {
    pub fn new(can: SocketCan, ecu: EcuDefinition) -> Self { Self { can, ecu } }

    pub fn ping(&self) -> Result<()> {
        self.retry("ping", || {
            self.send_command_wait_ack(&[protocol::GET], protocol::GET, COMMAND_TIMEOUT)
        })
    }

    pub fn request_bootloader(&self) -> Result<()> {
        let id = self.ecu.boot_request_can_id()?;
        let payload = self.ecu.boot_request_payload()?;
        self.can.send(id, &payload)?;
        thread::sleep(BOOT_DELAY);
        self.ping().context("failed to enter bootloader: no ping response")
    }

    pub fn get_version(&self) -> Result<BootInfo> {
        self.retry("get-version", || self.get_version_once())
    }

    pub fn start_application(&self) -> Result<()> {
        let mut request = vec![protocol::GO];
        request.extend_from_slice(&protocol::LOGICAL_APPLICATION_ADDRESS.to_be_bytes());
        self.send_command_wait_ack(&request, protocol::GO, COMMAND_TIMEOUT)
    }

    pub fn change_baud_rate(&self, bit_rate: u32) -> Result<()> {
        let request = [protocol::SET_BAUD_RATE, protocol::baud_rate_code(bit_rate)?];
        self.send_command_wait_ack(&request, protocol::SET_BAUD_RATE, COMMAND_TIMEOUT)
    }

    pub fn flash(&self, image: &Firmware, already_in_bootloader: bool) -> Result<()> {
        if !already_in_bootloader {
            print_progress(1.0, "Entering bootloader", None)?;
            self.request_bootloader()?;
        }

        print_progress(3.0, "Detecting bootloader", None)?;
        let _info = self.get_version()?;
        let target_address = protocol::INACTIVE_APPLICATION_ADDRESS;

        print_progress(5.0, "Starting update", None)?;
        self.retry("start-update", || self.start_update(image))?;

        print_progress(8.0, "Erasing inactive bank", None)?;
        self.retry("erase", || self.erase())?;

        let mut completed = 0usize;
        while completed < image.data.len() {
            let length = protocol::MAXIMUM_WRITE_SIZE.min(image.data.len() - completed);
            let block = &image.data[completed..completed + length];
            let address = target_address + completed as u32;
            self.retry_write_block(address, block)?;
            completed += length;

            let percent = 10.0 + 80.0 * completed as f64 / image.data.len() as f64;
            let detail = format!("{completed}/{} bytes", image.data.len());
            print_progress(percent, "Programming", Some(&detail))?;
        }

        print_progress(92.0, "Verifying CRC", None)?;
        let returned_crc = self.retry("compute-crc", || self.verify())?;
        anyhow::ensure!(
            returned_crc == image.crc32,
            "bootloader returned CRC 0x{returned_crc:08X}; expected 0x{:08X}",
            image.crc32
        );

        print_progress(97.0, "Swapping banks", None)?;
        self.swap_banks()?;

        print_progress(100.0, "Complete", None)?;
        println!();
        Ok(())
    }

    fn get_version_once(&self) -> Result<BootInfo> {
        self.can.clear()?;
        self.can.send(self.ecu.request_can_id()?, &[protocol::GET_VERSION])?;
        self.wait_ack(protocol::GET_VERSION, COMMAND_TIMEOUT)?;
        let response = self.wait_response(protocol::GET_VERSION, 4, COMMAND_TIMEOUT)?;
        Ok(BootInfo {
            version: response.data[1],
            bank_swap_enabled: response.data[2] != 0,
        })
    }

    fn start_update(&self, image: &Firmware) -> Result<()> {
        anyhow::ensure!(image.data.len() <= 0xFF_FFFF, "image size exceeds bootloader protocol limit");
        let len = image.data.len() as u32;
        let request = [
            protocol::START_UPDATE,
            (len >> 16) as u8,
            (len >> 8) as u8,
            len as u8,
            (image.crc32 >> 24) as u8,
            (image.crc32 >> 16) as u8,
            (image.crc32 >> 8) as u8,
            image.crc32 as u8,
        ];
        self.send_command_wait_ack(&request, protocol::START_UPDATE, COMMAND_TIMEOUT)
    }

    fn erase(&self) -> Result<()> {
        self.send_command_wait_ack(
            &[protocol::ERASE_MEMORY, protocol::FIRST_APPLICATION_SECTOR, protocol::APPLICATION_SECTOR_COUNT],
            protocol::ERASE_MEMORY,
            ERASE_TIMEOUT,
        )
    }

    fn retry_write_block(&self, address: u32, data: &[u8]) -> Result<()> {
        for attempt in 1..=MAX_ATTEMPTS {
            match self.write_block(address, data)? {
                AckStatus::Ack => return Ok(()),
                AckStatus::Nack => {
                    if attempt < MAX_ATTEMPTS {
                        thread::sleep(RETRY_DELAY);
                    }
                }
                AckStatus::Timeout => {
                    anyhow::bail!("write block timed out");
                }
            }
        }

        anyhow::bail!(
            "write block rejected after {MAX_ATTEMPTS} attempts"
        )
    }

    fn write_block(&self, address: u32, data: &[u8]) -> Result<AckStatus> {
        anyhow::ensure!(
            data.len() <= protocol::MAXIMUM_WRITE_SIZE,
            "write block too large"
        );

        let length = u16::try_from(data.len())?;
        let mut request = [0u8; 7];
        request[0] = protocol::WRITE_MEMORY;
        request[1..5].copy_from_slice(&address.to_be_bytes());
        request[5..7].copy_from_slice(&length.to_be_bytes());

        self.can.clear()?;

        match self.retry_write_request(&request)? {
            AckStatus::Ack => {}
            AckStatus::Nack => return Ok(AckStatus::Nack),
            AckStatus::Timeout => {
                anyhow::bail!(
                    "timed out waiting for initial WRITE_MEMORY ACK after retries"
                )
            }
        }

        let mut offset = 0usize;
        let mut sequence = 0u8;

        while offset < data.len() {
            let payload_length =
                protocol::WRITE_FRAME_PAYLOAD_SIZE.min(data.len() - offset);

            let mut frame = Vec::with_capacity(payload_length + 1);
            frame.push(sequence);
            frame.extend_from_slice(
                &data[offset..offset + payload_length]
            );

            match self.retry_write_packet(sequence, &frame)? {
                AckStatus::Ack => {}
                AckStatus::Nack => return Ok(AckStatus::Nack),
                AckStatus::Timeout => {
                    anyhow::bail!(
                        "write packet sequence {sequence} timed out after                          {MAX_WRITE_PACKET_ATTEMPTS} attempts"
                    )
                }
            }

            offset += payload_length;
            sequence = sequence.wrapping_add(1);
        }

        match self.wait_ack_status(protocol::WRITE_MEMORY, COMMAND_TIMEOUT)? {
            AckStatus::Ack => Ok(AckStatus::Ack),
            AckStatus::Nack => Ok(AckStatus::Nack),
            AckStatus::Timeout => {
                anyhow::bail!(
                    "timed out waiting for final WRITE_MEMORY ACK"
                )
            }
        }
    }

    fn retry_write_request(&self, request: &[u8]) -> Result<AckStatus> {
        for attempt in 1..=MAX_WRITE_PACKET_ATTEMPTS {
            self.can.send(self.ecu.request_can_id()?, request)?;

            match self.wait_ack_status(
                protocol::WRITE_MEMORY,
                COMMAND_TIMEOUT,
            )? {
                AckStatus::Ack => return Ok(AckStatus::Ack),
                AckStatus::Nack => return Ok(AckStatus::Nack),
                AckStatus::Timeout => {
                    if attempt < MAX_WRITE_PACKET_ATTEMPTS {
                        thread::sleep(RETRY_DELAY);
                    }
                }
            }
        }

        Ok(AckStatus::Timeout)
    }

    fn retry_write_packet(
        &self,
        sequence: u8,
        frame: &[u8],
    ) -> Result<AckStatus> {
        for attempt in 1..=MAX_WRITE_PACKET_ATTEMPTS {
            self.can.send(self.ecu.write_data_can_id()?, frame)?;

            match self.wait_write_packet_ack(
                sequence,
                PACKET_TIMEOUT,
            )? {
                AckStatus::Ack => return Ok(AckStatus::Ack),
                AckStatus::Nack => return Ok(AckStatus::Nack),
                AckStatus::Timeout => {
                    if attempt < MAX_WRITE_PACKET_ATTEMPTS {
                        thread::sleep(RETRY_DELAY);
                    }
                }
            }
        }

        Ok(AckStatus::Timeout)
    }

    fn verify(&self) -> Result<u32> {
        self.can.clear()?;
        self.can.send(self.ecu.request_can_id()?, &[protocol::COMPUTE_CRC])?;
        self.wait_ack(protocol::COMPUTE_CRC, COMMAND_TIMEOUT)?;
        let response = self.wait_response(protocol::COMPUTE_CRC, 5, COMMAND_TIMEOUT)?;
        Ok(u32::from_be_bytes(response.data[1..5].try_into().unwrap()))
    }

    fn swap_banks(&self) -> Result<()> {
        self.send_command_wait_ack(&[protocol::BANK_SWAP], protocol::BANK_SWAP, COMMAND_TIMEOUT)
    }

    fn send_command_wait_ack(&self, request: &[u8], command: u8, timeout: Duration) -> Result<()> {
        self.can.clear()?;
        self.can.send(self.ecu.request_can_id()?, request)?;
        self.wait_ack(command, timeout)
    }

    fn wait_write_packet_ack(
        &self,
        sequence: u8,
        timeout: Duration,
    ) -> Result<AckStatus> {
        let response_id = self.ecu.response_can_id()?;
        let deadline = Instant::now() + timeout;

        while Instant::now() < deadline {
            let remaining =
                deadline.saturating_duration_since(Instant::now());

            let Some(frame) = self.can.receive(remaining)? else {
                break;
            };

            if frame.id != response_id
                || frame.data.len() < 2
                || frame.data[1] != protocol::WRITE_MEMORY
            {
                continue;
            }

            if frame.data[0] == protocol::NACK {
                return Ok(AckStatus::Nack);
            }

            if frame.data.len() == 3
                && frame.data[0] == protocol::ACK
                && frame.data[2] == sequence
            {
                return Ok(AckStatus::Ack);
            }
        }

        Ok(AckStatus::Timeout)
    }

    fn wait_ack(&self, command: u8, timeout: Duration) -> Result<()> {
        match self.wait_ack_status(command, timeout)? {
            AckStatus::Ack => Ok(()),
            AckStatus::Nack => {
                anyhow::bail!(
                    "bootloader rejected command 0x{command:02X}"
                )
            }
            AckStatus::Timeout => {
                anyhow::bail!(
                    "timed out waiting for ACK to command 0x{command:02X}"
                )
            }
        }
    }

    fn wait_ack_status(
        &self,
        command: u8,
        timeout: Duration,
    ) -> Result<AckStatus> {
        let response_id = self.ecu.response_can_id()?;
        let deadline = Instant::now() + timeout;

        while Instant::now() < deadline {
            let remaining =
                deadline.saturating_duration_since(Instant::now());

            let Some(frame) = self.can.receive(remaining)? else {
                break;
            };

            if frame.id != response_id
                || frame.data.len() != 2
                || frame.data[1] != command
            {
                continue;
            }

            match frame.data[0] {
                protocol::ACK => return Ok(AckStatus::Ack),
                protocol::NACK => return Ok(AckStatus::Nack),
                _ => {}
            }
        }

        Ok(AckStatus::Timeout)
    }

    fn wait_response(&self, command: u8, minimum_length: usize, timeout: Duration) -> Result<CanFrame> {
        let response_id = self.ecu.response_can_id()?;
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let Some(frame) = self.can.receive(remaining)? else { break; };
            if frame.id == response_id && frame.data.len() >= minimum_length && frame.data[0] == command {
                return Ok(frame);
            }
        }
        anyhow::bail!("timed out waiting for response to command 0x{command:02X}")
    }

    fn retry<T, F>(&self, name: &str, mut operation: F) -> Result<T>
    where F: FnMut() -> Result<T> {
        let mut last = None;
        for attempt in 1..=MAX_ATTEMPTS {
            match operation() {
                Ok(value) => return Ok(value),
                Err(error) => {
                    last = Some(error);
                    if attempt < MAX_ATTEMPTS {
                        thread::sleep(RETRY_DELAY);
                    }
                }
            }
        }
        Err(last.unwrap()).with_context(|| format!("{name} failed after {MAX_ATTEMPTS} attempts"))
    }
}

fn print_progress(percent: f64, stage: &str, detail: Option<&str>) -> Result<()> {
    const BAR_WIDTH: usize = 24;

    let percent = percent.clamp(0.0, 100.0);
    let filled = ((percent / 100.0) * BAR_WIDTH as f64).round() as usize;
    let empty = BAR_WIDTH.saturating_sub(filled);
    let bar = format!("{}{}", "#".repeat(filled), "-".repeat(empty));

    match detail {
        Some(detail) => print!("\r\x1b[2K[{bar}] {percent:5.1}%  {stage:<20} {detail}"),
        None => print!("\r\x1b[2K[{bar}] {percent:5.1}%  {stage}"),
    }
    io::stdout().flush().context("failed to update progress display")?;
    Ok(())
}

