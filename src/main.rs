#![deny(warnings)]

mod can;
mod config;
mod firmware;
mod flash;
mod protocol;
mod server;
mod types;

use anyhow::{Context, Result};
use can::SocketCan;
use clap::{Parser, Subcommand};
use flash::FirmwareFlashManager;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "ajax",
    about = "Firmware update application for vehicle ECUs over CAN. Starts the HTTP server when no command is supplied."
)]
struct Cli {
    /// CAN interface.
    #[arg(long, global = true, default_value = "can0")]
    interface: String,

    /// Path to ECU config file.
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// ECU name from config (for example BMS or VCU)
    #[arg(long)]
    ecu: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Ping the bootloader
    Ping,
    /// Read bootloader version
    Version,
    /// Request application -> bootloader transition, then verify with ping
    EnterBootloader,
    /// Start the logical application image
    StartApp,
    /// Change the bootloader CAN bitrate (Linux interface must be reconfigured separately)
    SetBaud { bit_rate: u32 },
    /// Program, CRC-verify, and activate a firmware image
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

    let executable_path = std::env::current_exe().context("failed to determine executable path")?;
    let executable_directory = executable_path
        .parent()
        .context("failed to determine executable directory")?;
    let config_path = cli
        .config
        .unwrap_or_else(|| executable_directory.join("ecus.json"));

    let config_file = config::load(&config_path)?;
    let interface = cli.interface;
    let Some(command) = cli.command else {
        anyhow::ensure!(
            cli.ecu.is_none(),
            "server mode selects the ECU from each upload; omit --ecu or specify a CAN command"
        );
        return server::run(config_file, interface);
    };
    let ecu_name = cli
        .ecu
        .as_deref()
        .context("--ecu is required for CAN commands")?;
    let ecu = config::select_ecu(&config_file, ecu_name)?;
    let ecu_display = ecu_name.to_ascii_uppercase();

    let can = SocketCan::open(&interface)
        .with_context(|| format!("failed to open CAN interface {interface}"))?;
    let flash_manager = FirmwareFlashManager::new(can, ecu.clone());

    match command {
        Command::Ping => {
            flash_manager.ping()?;
            println!("{ecu_display} bootloader responded");
        }
        Command::Version => {
            let status = flash_manager.get_status()?;
            let major = status.version >> 4;
            let minor = status.version & 0x0F;
            println!("Bootloader version: {major}.{minor}");
        }
        Command::EnterBootloader => {
            flash_manager.request_bootloader()?;
            println!("{ecu_display} entered bootloader");
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
            println!("{ecu_display} flash request received");
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
