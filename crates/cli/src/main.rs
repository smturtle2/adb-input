// SPDX-License-Identifier: EUPL-1.2
mod adb;
mod control;
mod input;
mod interactive;
mod keys;
mod terminal;
use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use control::Control;
use std::{path::PathBuf, process::Command};

#[derive(Parser)]
#[command(
    version,
    about = "Share desktop keyboard and mouse with Android over ADB"
)]
struct Cli {
    /// adb executable (otherwise resolved through PATH)
    #[arg(long, global = true, default_value = "adb")]
    adb: PathBuf,
    #[command(subcommand)]
    command: Option<Action>,
}
#[derive(Subcommand)]
enum Action {
    /// Pair Android wireless debugging; enter the code at the prompt
    Pair { address: String },
    /// Connect over TCP, including a VPN address
    Connect { address: String },
    /// Show ADB devices
    Devices,
    /// Diagnose ADB availability and desktop input permissions
    Doctor,
    /// Forward input; Ctrl+Shift+R toggles between desktop and Android
    Run {
        /// ADB serial; automatically selected when exactly one device is online
        #[arg(short, long)]
        device: Option<String>,
        /// Connect to this TCP endpoint before starting
        #[arg(long, conflicts_with = "device")]
        connect: Option<String>,
        /// Override the embedded Android agent (development only)
        #[arg(long)]
        agent: Option<PathBuf>,
        /// Mouse movement multiplier
        #[arg(long, default_value_t = 1.0)]
        sensitivity: f64,
    },
    /// Create virtual input devices and check the agent, without capturing desktop input
    Probe {
        #[arg(short, long)]
        device: Option<String>,
        #[arg(long)]
        agent: Option<PathBuf>,
    },
}
fn forward(adb: &std::path::Path, args: &[&str]) -> Result<()> {
    let status = Command::new(adb)
        .args(args)
        .status()
        .context("cannot run adb; install Android platform-tools or use --adb")?;
    if !status.success() {
        bail!("adb exited with {status}");
    }
    Ok(())
}
fn run() -> Result<()> {
    let cli = Cli::parse();
    let control = Control::new()?;
    let Some(command) = cli.command else {
        return interactive::run(&cli.adb, &control);
    };
    match command {
        Action::Pair { address } => forward(&cli.adb, &["pair", &address]),
        Action::Connect { address } => {
            adb::connect(&cli.adb, &address)?;
            Ok(())
        }
        Action::Devices => forward(&cli.adb, &["devices", "-l"]),
        Action::Doctor => {
            forward(&cli.adb, &["version"])?;
            input::doctor()
        }
        Action::Probe { device, agent } => {
            let serial = adb::select(&cli.adb, device)?;
            let mut connection = adb::Connection::start(&cli.adb, &serial, agent.as_deref())?;
            connection.heartbeat()?;
            eprintln!("Agent and both UHID devices ready; probe finished.");
            Ok(())
        }
        Action::Run {
            device,
            connect,
            agent,
            sensitivity,
        } => {
            if !sensitivity.is_finite() || !(0.01..=100.0).contains(&sensitivity) {
                bail!("sensitivity must be between 0.01 and 100");
            }
            let serial = match connect {
                Some(address) => {
                    adb::connect(&cli.adb, &address)?;
                    address
                }
                None => adb::select(&cli.adb, device)?,
            };
            control.session(&cli.adb, &serial, agent.as_deref(), sensitivity)
        }
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("adb-input: {error:#}");
        std::process::exit(1);
    }
}
