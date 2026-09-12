#![deny(warnings)]

mod config;
mod firmware;
mod flash;
mod protocol;
mod can;
mod types;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use flash::FirmwareFlashManager;
use can::SocketCan;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "ajax", about = "Firmware update application for vehicle ECUs over CAN")]
struct Cli {
    /// CAN interface.
    #[arg(long, global = true, default_value = "can0")]
    interface: String,

    /// Path to ECU config file.
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// ECU name from config (for example BMS or VCU)
    #[arg(long)]
    ecu: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Ping the bootloader
    Ping,
    /// Read bootloader version and bank swap state
    Version,
    /// Request application -> bootloader transition, then verify with ping
    EnterBootloader,
    /// Start the logical application image
    StartApp,
    /// Change the bootloader CAN bitrate (Linux interface must be reconfigured separately)
    SetBaud { bit_rate: u32 },
    /// Program, CRC-verify, and bank-swap a firmware image
    Flash {
        file: PathBuf,
        #[arg(long)]
        already_in_bootloader: bool,
    },
}

fn main() {
    if let Err(error) = run() {
        eprintln!("\nerror: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();

    let executable_path = std::env::current_exe()
        .context("failed to determine executable path")?;
    let executable_directory = executable_path
        .parent()
        .context("failed to determine executable directory")?;
    let config_path = cli.config.unwrap_or_else(|| executable_directory.join("ecus.json"));

    let config_file = config::load(&config_path)?;
    let ecu = config::select_ecu(&config_file, &cli.ecu)?;

    let interface = cli.interface;

    let can = SocketCan::open(&interface)
        .with_context(|| format!("failed to open CAN interface {interface}"))?;
    let flash_manager = FirmwareFlashManager::new(can, ecu.clone());

    match cli.command {
        Command::Ping => {
            flash_manager.ping()?;
            println!("{} bootloader responded", ecu.label);
        }
        Command::Version => {
            let info = flash_manager.get_version()?;
            let major = info.version >> 4;
            let minor = info.version & 0x0F;

            println!("Bootloader version: {major}.{minor}");
            println!(
                "Bank swap: {}",
                if info.bank_swap_enabled {
                    "Enabled"
                } else {
                    "Disabled"
                }
            );
        }
        Command::EnterBootloader => {
            flash_manager.request_bootloader()?;
            println!("{} entered bootloader", ecu.label);
        }
        Command::StartApp => {
            flash_manager.start_application()?;
            println!("start-application command acknowledged");
        }
        Command::SetBaud { bit_rate } => {
            anyhow::ensure!(
                config_file.supported_bit_rates.contains(&bit_rate),
                "bitrate {bit_rate} is not listed in config"
            );
            flash_manager.change_baud_rate(bit_rate)?;
            println!("bootloader accepted {bit_rate} bit/s");
            println!("reconfigure {interface} to the same bitrate before sending more CAN traffic");
        }
        Command::Flash {
            file,
            already_in_bootloader,
        } => {
            let image = firmware::load(&file)?;
            println!("Firmware: {}", file.display());
            println!("Format:   {}", image.format);
            println!("Address:  0x{:08X}", image.address);
            println!("Size:     {} bytes", image.data.len());
            println!("CRC32:    0x{:08X}", image.crc32);
            flash_manager.flash(&image, already_in_bootloader)?;
        }
    }
    Ok(())
}
