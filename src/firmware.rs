use crate::{protocol, types::Firmware};
use anyhow::{Context, Result};
use std::{collections::BTreeMap, fs, path::Path};

const FLASH_START: u32 = 0x0800_0000;
const FLASH_END: u32 = 0x0820_0000;
const CRC_POLYNOMIAL: u32 = 0x04C1_1DB7;

pub fn load(path: &Path) -> Result<Firmware> {
    let extension = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    let firmware = match extension.as_str() {
        "bin" => load_bin(path)?,
        "hex" | "ihex" => create_mapped(path, "HEX", load_hex(path)?)?,
        "elf" | "axf" => create_mapped(path, "ELF", load_elf(path)?)?,
        _ if has_elf_magic(path)? => create_mapped(path, "ELF", load_elf(path)?)?,
        _ => anyhow::bail!(
            "unsupported firmware format; expected .bin, .hex, .ihex, .elf, .axf, or an extensionless ELF binary"
        ),
    };

    validate(&firmware)?;
    Ok(firmware)
}

fn has_elf_magic(path: &Path) -> Result<bool> {
    use std::io::Read;

    let mut file = fs::File::open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    let mut magic = [0u8; 4];
    let bytes_read = file
        .read(&mut magic)
        .with_context(|| format!("failed to read {}", path.display()))?;

    Ok(bytes_read == magic.len() && magic == *b"\x7FELF")
}

fn load_bin(path: &Path) -> Result<Firmware> {
    let data = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    anyhow::ensure!(!data.is_empty(), "BIN file is empty");
    Ok(Firmware {
        crc32: crc32_stm32(&data),
        data,
        address: protocol::LOGICAL_APPLICATION_ADDRESS,
        format: "BIN",
    })
}

fn load_hex(path: &Path) -> Result<BTreeMap<u32, u8>> {
    let text = fs::read_to_string(path)?;
    let mut memory = BTreeMap::new();
    let mut address_base = 0u32;

    for (line_index, source) in text.lines().enumerate() {
        let line = source.trim();
        if line.is_empty() { continue; }
        anyhow::ensure!(line.starts_with(':'), "invalid HEX record at line {}", line_index + 1);
        let raw = decode_hex(&line[1..]).with_context(|| format!("invalid HEX at line {}", line_index + 1))?;
        anyhow::ensure!(raw.len() >= 5, "HEX record too short at line {}", line_index + 1);
        let length = raw[0] as usize;
        anyhow::ensure!(raw.len() == length + 5, "HEX length mismatch at line {}", line_index + 1);
        let checksum = raw.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte));
        anyhow::ensure!(checksum == 0, "HEX checksum failed at line {}", line_index + 1);

        let offset = u16::from_be_bytes([raw[1], raw[2]]) as u32;
        let record_type = raw[3];
        let payload = &raw[4..4 + length];

        match record_type {
            0x00 => {
                let address = address_base + offset;
                for (i, value) in payload.iter().enumerate() {
                    insert_byte(&mut memory, address + i as u32, *value)?;
                }
            }
            0x01 => break,
            0x02 => {
                anyhow::ensure!(payload.len() == 2, "invalid extended segment record");
                address_base = (u16::from_be_bytes([payload[0], payload[1]]) as u32) << 4;
            }
            0x04 => {
                anyhow::ensure!(payload.len() == 2, "invalid extended linear record");
                address_base = (u16::from_be_bytes([payload[0], payload[1]]) as u32) << 16;
            }
            0x03 | 0x05 => {}
            other => anyhow::bail!("unsupported HEX record type 0x{other:02X}"),
        }
    }
    Ok(memory)
}

fn load_elf(path: &Path) -> Result<BTreeMap<u32, u8>> {
    let elf = fs::read(path)?;
    anyhow::ensure!(elf.len() >= 52, "ELF file is too short");
    anyhow::ensure!(&elf[0..4] == b"\x7FELF", "invalid ELF magic");
    anyhow::ensure!(elf[4] == 1, "only 32-bit ELF is supported");
    anyhow::ensure!(elf[5] == 1, "only little-endian ELF is supported");
    anyhow::ensure!(u16_le(&elf, 18)? == 40, "ELF is not ARM");

    let phoff = u32_le(&elf, 28)? as usize;
    let phentsize = u16_le(&elf, 42)? as usize;
    let phnum = u16_le(&elf, 44)? as usize;
    anyhow::ensure!(phentsize >= 32, "invalid ELF program header size");

    let mut memory = BTreeMap::new();
    for index in 0..phnum {
        let off = phoff.checked_add(index * phentsize).ok_or_else(|| anyhow::anyhow!("ELF header overflow"))?;
        anyhow::ensure!(off + phentsize <= elf.len(), "ELF program header outside file");
        let h = &elf[off..off + phentsize];
        if u32::from_le_bytes(h[0..4].try_into().unwrap()) != 1 { continue; }

        let file_offset = u32::from_le_bytes(h[4..8].try_into().unwrap()) as usize;
        let virtual_address = u32::from_le_bytes(h[8..12].try_into().unwrap());
        let physical_address = u32::from_le_bytes(h[12..16].try_into().unwrap());
        let file_size = u32::from_le_bytes(h[16..20].try_into().unwrap()) as usize;
        let address = if physical_address != 0 { physical_address } else { virtual_address };

        if file_size == 0 || address < FLASH_START || address >= FLASH_END { continue; }
        anyhow::ensure!(file_offset + file_size <= elf.len(), "ELF loadable segment outside file");

        for (i, value) in elf[file_offset..file_offset + file_size].iter().enumerate() {
            insert_byte(&mut memory, address + i as u32, *value)?;
        }
    }
    Ok(memory)
}

fn create_mapped(_path: &Path, format: &'static str, memory: BTreeMap<u32, u8>) -> Result<Firmware> {
    let first = *memory.keys().next().ok_or_else(|| anyhow::anyhow!("firmware contains no flash data"))?;
    let last = *memory.keys().next_back().unwrap();
    anyhow::ensure!(first == protocol::LOGICAL_APPLICATION_ADDRESS,
        "firmware starts at 0x{first:08X}; expected 0x{:08X}", protocol::LOGICAL_APPLICATION_ADDRESS);

    let mut data = vec![0xFFu8; (last - first + 1) as usize];
    for (address, value) in memory {
        data[(address - first) as usize] = value;
    }
    Ok(Firmware { crc32: crc32_stm32(&data), data, address: first, format })
}

fn validate(firmware: &Firmware) -> Result<()> {
    anyhow::ensure!(firmware.data.len() >= 8, "firmware does not contain a complete vector table");
    anyhow::ensure!(firmware.address == protocol::LOGICAL_APPLICATION_ADDRESS, "firmware is linked at the wrong application address");
    anyhow::ensure!(firmware.data.len() <= protocol::MAXIMUM_IMAGE_SIZE, "firmware is larger than the application region");

    let stack_pointer = u32::from_le_bytes(firmware.data[0..4].try_into().unwrap());
    let reset_handler = u32::from_le_bytes(firmware.data[4..8].try_into().unwrap());
    let reset_address = reset_handler & !1;
    let stack_valid = (0x2000_0000..=0x2009_FFF0).contains(&stack_pointer);
    let reset_valid = (reset_handler & 1) != 0
        && reset_address >= protocol::LOGICAL_APPLICATION_ADDRESS
        && reset_address < protocol::LOGICAL_APPLICATION_ADDRESS + protocol::MAXIMUM_IMAGE_SIZE as u32;
    anyhow::ensure!(stack_valid && reset_valid, "firmware vector table is not valid for STM32H563");
    Ok(())
}

pub fn crc32_stm32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &value in data {
        crc ^= (value as u32) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 { (crc << 1) ^ CRC_POLYNOMIAL } else { crc << 1 };
        }
    }
    crc
}

fn insert_byte(memory: &mut BTreeMap<u32, u8>, address: u32, value: u8) -> Result<()> {
    if let Some(old) = memory.insert(address, value) {
        anyhow::ensure!(old == value, "conflicting firmware bytes at 0x{address:08X}");
    }
    Ok(())
}

fn decode_hex(text: &str) -> Result<Vec<u8>> {
    anyhow::ensure!(text.len() % 2 == 0, "odd hex length");
    let mut out = Vec::with_capacity(text.len() / 2);
    for i in (0..text.len()).step_by(2) {
        out.push(u8::from_str_radix(&text[i..i + 2], 16)?);
    }
    Ok(out)
}

fn u16_le(data: &[u8], offset: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(data.get(offset..offset + 2).ok_or_else(|| anyhow::anyhow!("ELF truncated"))?.try_into().unwrap()))
}
fn u32_le(data: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(data.get(offset..offset + 4).ok_or_else(|| anyhow::anyhow!("ELF truncated"))?.try_into().unwrap()))
}
