//! Loads ELF, HEX, and BIN firmware and checks it fits the ECU's flash.

use crate::types::{Firmware, PlatformDefinition};
use anyhow::{Context, Result};
use std::{collections::BTreeMap, fs, path::Path};

const CRC_POLYNOMIAL: u32 = 0x04C1_1DB7;

/// Loads an image by file extension, or by ELF header if there is none, and validates it.
pub fn load(path: &Path, platform: &PlatformDefinition) -> Result<Firmware> {
    let extension = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    let firmware = match extension.as_str() {
        "bin" => load_bin(path, platform)?,
        "hex" | "ihex" => create_mapped(platform, "HEX", load_hex(path, platform)?)?,
        "elf" | "axf" => create_mapped(platform, "ELF", load_elf(path, platform)?)?,
        _ if has_elf_magic(path)? => create_mapped(platform, "ELF", load_elf(path, platform)?)?,
        _ => anyhow::bail!(
            "unsupported firmware format; expected .bin, .hex, .ihex, .elf, .axf, or an extensionless ELF binary"
        ),
    };

    validate(&firmware, platform)?;
    Ok(firmware)
}

fn has_elf_magic(path: &Path) -> Result<bool> {
    use std::io::Read;

    let mut file =
        fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut magic = [0u8; 4];
    let bytes_read = file
        .read(&mut magic)
        .with_context(|| format!("failed to read {}", path.display()))?;

    Ok(bytes_read == magic.len() && magic == *b"\x7FELF")
}

/// Loads a raw binary, placed at the start of the flash region.
fn load_bin(path: &Path, platform: &PlatformDefinition) -> Result<Firmware> {
    let data = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    anyhow::ensure!(!data.is_empty(), "BIN file is empty");
    Ok(Firmware {
        crc32: crc32_stm32(&data),
        data,
        address: platform.flash.address,
        format: "BIN",
    })
}

/// Parses Intel HEX records into bytes by address.
fn load_hex(path: &Path, platform: &PlatformDefinition) -> Result<BTreeMap<u32, u8>> {
    let text = String::from_utf8(fs::read(path)?)
        .ok()
        .context("HEX file is not text")?;
    let mut memory = BTreeMap::new();
    let mut address_base = 0u32;
    let mut has_end_record = false;

    for (line_index, source) in text.lines().enumerate() {
        let line = source.trim();
        if line.is_empty() {
            continue;
        }
        anyhow::ensure!(
            line.starts_with(':'),
            "invalid HEX record at line {}",
            line_index + 1
        );
        let raw = decode_hex(&line[1..])
            .with_context(|| format!("invalid HEX at line {}", line_index + 1))?;
        anyhow::ensure!(
            raw.len() >= 5,
            "HEX record too short at line {}",
            line_index + 1
        );
        let length = raw[0] as usize;
        anyhow::ensure!(
            raw.len() == length + 5,
            "HEX length mismatch at line {}",
            line_index + 1
        );
        let checksum = raw.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte));
        anyhow::ensure!(
            checksum == 0,
            "HEX checksum failed at line {}",
            line_index + 1
        );

        let offset = u16::from_be_bytes([raw[1], raw[2]]) as u32;
        let record_type = raw[3];
        let payload = &raw[4..4 + length];

        match record_type {
            0x00 => {
                let address = address_base + offset;
                // Check before storing the data, so a large file cannot use up memory.
                anyhow::ensure!(
                    address >= platform.flash.address
                        && (address as u64 + length as u64) <= platform.flash.end() as u64,
                    "HEX data at 0x{address:08X} is outside the flash region"
                );
                for (i, value) in payload.iter().enumerate() {
                    insert_byte(&mut memory, address + i as u32, *value)?;
                }
            }
            0x01 => {
                has_end_record = true;
                break;
            }
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
    anyhow::ensure!(has_end_record, "HEX file is incomplete: no end record");
    Ok(memory)
}

/// Collects the bytes of loadable 32-bit ARM ELF segments by address.
fn load_elf(path: &Path, platform: &PlatformDefinition) -> Result<BTreeMap<u32, u8>> {
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
        let off = phoff
            .checked_add(index * phentsize)
            .ok_or_else(|| anyhow::anyhow!("ELF header overflow"))?;
        anyhow::ensure!(
            off + phentsize <= elf.len(),
            "ELF program header outside file"
        );
        let h = &elf[off..off + phentsize];
        if u32::from_le_bytes(h[0..4].try_into().unwrap()) != 1 {
            continue;
        }

        let file_offset = u32::from_le_bytes(h[4..8].try_into().unwrap()) as usize;
        let virtual_address = u32::from_le_bytes(h[8..12].try_into().unwrap());
        let physical_address = u32::from_le_bytes(h[12..16].try_into().unwrap());
        let file_size = u32::from_le_bytes(h[16..20].try_into().unwrap()) as usize;
        // Prefer the load address, where initialized data is stored in flash.
        let address = if physical_address != 0 {
            physical_address
        } else {
            virtual_address
        };

        // RAM-only segments are not flashed; everything else must fit the flash region.
        if file_size == 0 || (platform.ram.address..platform.ram.end()).contains(&address) {
            continue;
        }
        anyhow::ensure!(
            address >= platform.flash.address
                && (address as u64 + file_size as u64) <= platform.flash.end() as u64,
            "ELF segment at 0x{address:08X} is outside the flash region"
        );
        anyhow::ensure!(
            file_offset + file_size <= elf.len(),
            "ELF loadable segment outside file"
        );

        for (i, value) in elf[file_offset..file_offset + file_size].iter().enumerate() {
            insert_byte(&mut memory, address + i as u32, *value)?;
        }
    }
    Ok(memory)
}

/// Flattens bytes by address into one image from the flash start, filling gaps with 0xFF.
fn create_mapped(
    platform: &PlatformDefinition,
    format: &'static str,
    memory: BTreeMap<u32, u8>,
) -> Result<Firmware> {
    let first = *memory
        .keys()
        .next()
        .ok_or_else(|| anyhow::anyhow!("firmware contains no flash data"))?;
    let last = *memory.keys().next_back().unwrap();
    anyhow::ensure!(
        first == platform.flash.address,
        "firmware starts at 0x{first:08X}; expected 0x{:08X}",
        platform.flash.address
    );
    // Check before allocating the flat image.
    anyhow::ensure!(
        last < platform.flash.end(),
        "firmware is larger than the flash region"
    );

    let mut data = vec![0xFFu8; (last - first + 1) as usize];
    for (address, value) in memory {
        data[(address - first) as usize] = value;
    }
    Ok(Firmware {
        crc32: crc32_stm32(&data),
        data,
        address: first,
        format,
    })
}

/// Checks the image's address and size, and that its vector table points into RAM and flash.
fn validate(firmware: &Firmware, platform: &PlatformDefinition) -> Result<()> {
    anyhow::ensure!(
        firmware.data.len() >= 8,
        "firmware does not contain a complete vector table"
    );
    anyhow::ensure!(
        firmware.address == platform.flash.address,
        "firmware is linked at the wrong flash address"
    );
    anyhow::ensure!(
        firmware.data.len() <= platform.flash.size as usize,
        "firmware is larger than the flash region"
    );

    let stack_pointer = u32::from_le_bytes(firmware.data[0..4].try_into().unwrap());
    let reset_handler = u32::from_le_bytes(firmware.data[4..8].try_into().unwrap());
    let reset_address = reset_handler & !1;
    let stack_valid = (platform.ram.address..=platform.ram.end()).contains(&stack_pointer)
        && stack_pointer % 8 == 0;
    let reset_valid = (reset_handler & 1) != 0
        && (platform.flash.address..platform.flash.end()).contains(&reset_address);
    anyhow::ensure!(
        stack_valid && reset_valid,
        "firmware vector table is not valid for the ECU's platform"
    );
    Ok(())
}

/// CRC-32 as the bootloader computes it: polynomial 0x04C11DB7, initial 0xFFFFFFFF, no reflection.
pub fn crc32_stm32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &value in data {
        crc ^= (value as u32) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ CRC_POLYNOMIAL
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// Stores a byte, rejecting a different value already at that address.
fn insert_byte(memory: &mut BTreeMap<u32, u8>, address: u32, value: u8) -> Result<()> {
    if let Some(old) = memory.insert(address, value) {
        anyhow::ensure!(
            old == value,
            "conflicting firmware bytes at 0x{address:08X}"
        );
    }
    Ok(())
}

fn decode_hex(text: &str) -> Result<Vec<u8>> {
    anyhow::ensure!(text.is_ascii(), "non-ASCII hex characters");
    anyhow::ensure!(text.len().is_multiple_of(2), "odd hex length");
    let mut out = Vec::with_capacity(text.len() / 2);
    for i in (0..text.len()).step_by(2) {
        out.push(u8::from_str_radix(&text[i..i + 2], 16)?);
    }
    Ok(out)
}

fn u16_le(data: &[u8], offset: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(
        data.get(offset..offset + 2)
            .ok_or_else(|| anyhow::anyhow!("ELF truncated"))?
            .try_into()
            .unwrap(),
    ))
}
fn u32_le(data: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        data.get(offset..offset + 4)
            .ok_or_else(|| anyhow::anyhow!("ELF truncated"))?
            .try_into()
            .unwrap(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Region;
    use std::io::Write;

    const PLATFORM: PlatformDefinition = PlatformDefinition {
        flash: Region {
            address: 0x0801_0000,
            size: 0xF_0000,
        },
        ram: Region {
            address: 0x2000_0000,
            size: 0xA_0000,
        },
    };

    fn load_hex_bytes(bytes: &[u8]) -> Result<BTreeMap<u32, u8>> {
        let mut file = tempfile::Builder::new().suffix(".hex").tempfile()?;
        file.write_all(bytes)?;
        load_hex(file.path(), &PLATFORM)
    }

    fn load_hex_error(bytes: &[u8]) -> String {
        format!("{:#}", load_hex_bytes(bytes).unwrap_err())
    }

    #[test]
    fn rejects_non_ascii_hex() {
        let error = load_hex_error(":10000000é0\n".as_bytes());
        assert!(error.contains("non-ASCII hex characters"));
    }

    #[test]
    fn rejects_hex_that_is_not_text() {
        assert_eq!(load_hex_error(b"\x7fELF\xff\xfe"), "HEX file is not text");
    }

    #[test]
    fn rejects_hex_outside_flash() {
        for (hex, address) in [
            // Extended linear address 0xFFFF, then 4 bytes at 0xFFFE run past 0xFFFFFFFF.
            (
                ":02000004FFFFFC\n:04FFFE00AABBCCDDF1\n:00000001FF\n",
                "0xFFFFFFFE",
            ),
            // 4 bytes at 0x080FFFFE run past the end of flash.
            (
                ":02000004080FE3\n:04FFFE00AABBCCDDF1\n:00000001FF\n",
                "0x080FFFFE",
            ),
            // 4 bytes at 0x0800FFFE start before flash.
            (
                ":020000040800F2\n:04FFFE00AABBCCDDF1\n:00000001FF\n",
                "0x0800FFFE",
            ),
        ] {
            assert_eq!(
                load_hex_error(hex.as_bytes()),
                format!("HEX data at {address} is outside the flash region")
            );
        }
    }

    #[test]
    fn requires_hex_end_record() {
        // 4 bytes at 0x080FFFFC end exactly at the end of flash.
        let hex = ":02000004080FE3\n:04FFFC00AABBCCDDF3\n";
        assert_eq!(
            load_hex_error(hex.as_bytes()),
            "HEX file is incomplete: no end record"
        );
        let memory = load_hex_bytes(format!("{hex}:00000001FF\n").as_bytes()).unwrap();
        assert_eq!(memory.len(), 4);
        assert_eq!(memory[&0x080F_FFFF], 0xDD);
    }
}
