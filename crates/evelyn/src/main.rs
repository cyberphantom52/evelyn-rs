use clap::{Parser, Subcommand, ValueEnum};
use evelyn::engine::Output;
use serde::Serialize;
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};
use thiserror::Error;
use tracing_subscriber::{EnvFilter, filter::LevelFilter};

#[derive(Parser)]
#[command(name = "evelyn", version, about = "Android firmware composition")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Set up a device repository, which ports one ROM to one device, and check
    /// what builds need, as `check` does.
    Init {
        /// The repository's directory [default: the current directory].
        dir: Option<PathBuf>,
    },
    /// Build the device repository the current directory is in into partition
    /// images, a super image, or a recovery-flashable ZIP.
    Build {
        #[command(flatten)]
        inputs: InputArgs,
        /// [default: super with a super layout in evelyn.toml, else partitions]
        #[arg(long, value_enum)]
        output: Option<OutputArg>,
        /// The build's working space [default: .evelyn in the repository].
        #[arg(long)]
        work_dir: Option<PathBuf>,
        /// Where the built images or ZIP are written [default: out/<port> in the repository].
        #[arg(long)]
        output_dir: Option<PathBuf>,
        /// An extra patch directory, applied after the port's.
        #[arg(long = "patch")]
        patches: Vec<PathBuf>,
        /// Apply every recipe to the firmware but write no images, and print what
        /// the build composed.
        #[arg(long)]
        dry_run: bool,
        /// Make a debug build: boot logs in /data/local/tmp/logs and adb on from
        /// boot without authorisation, applied after every other patch.
        #[arg(long)]
        debug: bool,
        /// Start SELinux permissive, through vendor_boot's bootconfig, applied
        /// after every other patch. init honours it only on an eng or userdebug
        /// build.
        #[arg(long)]
        selinux_permissive: bool,
        /// Leave the ART files an edit made stale (the boot image, odex, vdex
        /// and app images) as they are rather than compile them again, for a
        /// quicker build: ART does not use them, and the code they are for runs
        /// uncompiled.
        #[arg(long)]
        no_dexpreopt: bool,
    },
    /// Replace the OTA package in firmware/rom or firmware/stock, or both, by its
    /// images, so builds read them in place instead of extracting the package
    /// each time. Images an earlier extraction left there are replaced.
    Extract {
        #[arg(value_enum)]
        firmware: Option<FirmwareArg>,
    },
    /// Download every file builds need that Evelyn lacks, check those it has,
    /// and generate the signing key if there is none.
    Check,
}

/// What a build is of, where it is not the repository's default.
#[derive(clap::Args)]
struct InputArgs {
    /// The ROM being ported, which supplies the system side: a directory of
    /// <partition>.img images, or an OTA package, local or an http(s) URL
    /// [default: firmware/rom].
    #[arg(long)]
    rom: Option<String>,
    /// The device's own firmware, which supplies the vendor side, likewise
    /// [default: firmware/stock].
    #[arg(long)]
    stock: Option<String>,
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum FirmwareArg {
    Rom,
    Stock,
}

#[derive(Clone, Copy, ValueEnum)]
enum OutputArg {
    Partitions,
    Super,
    Zip,
}

impl From<OutputArg> for Output {
    fn from(value: OutputArg) -> Self {
        match value {
            OutputArg::Partitions => Self::Partitions,
            OutputArg::Super => Self::Super,
            OutputArg::Zip => Self::Zip,
        }
    }
}

#[derive(Debug, Error)]
enum Error {
    #[error(transparent)]
    Engine(#[from] evelyn::engine::Error),
    #[error(transparent)]
    Profile(#[from] evelyn::engine::profile::Error),
    #[error(transparent)]
    Toolchain(#[from] evelyn::toolchain::Error),
    #[error("{0} of what builds need could not be set up")]
    Unchecked(usize),
    #[error("cannot write JSON result")]
    Json(#[from] serde_json::Error),
    #[error("cannot write output")]
    Output(#[from] io::Error),
}

/// An error's message, then each of its causes': every error says what it was
/// doing and leaves its cause to its source. A cause whose message the one
/// before it already ends with, as some libraries write theirs, is left out.
fn messages(error: &dyn std::error::Error) -> Vec<String> {
    let mut messages = vec![error.to_string()];
    let mut cause = error.source();
    while let Some(error) = cause {
        let message = error.to_string();
        if !messages.last().is_some_and(|last| last.ends_with(&message)) {
            messages.push(message);
        }
        cause = error.source();
    }
    messages
}

/// What builds need beyond Evelyn, each with its state: the pinned files
/// and the signing key, printed whether or not some fail.
fn check(tools: &evelyn::toolchain::Tools) -> Result<serde_json::Value, Error> {
    let mut failed = 0;
    let mut checked = vec![];
    for status in tools.check() {
        let (state, error) = match status.state {
            Ok(evelyn::toolchain::State::Present) => ("present", None),
            Ok(evelyn::toolchain::State::New) => ("new", None),
            Err(error) => {
                failed += 1;
                ("failed", Some(messages(&error).join(": ")))
            }
        };
        checked.push(serde_json::json!({
            "name": status.name, "path": status.path, "state": state, "error": error,
        }));
    }
    let checked = serde_json::Value::from(checked);
    if failed > 0 {
        print(&checked)?;
        return Err(Error::Unchecked(failed));
    }
    Ok(checked)
}

fn print(value: &impl Serialize) -> Result<(), Error> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, value)?;
    writeln!(stdout)?;
    Ok(())
}

fn run(cli: Cli) -> Result<(), Error> {
    match cli.command {
        Command::Init { dir } => {
            let repository = evelyn::engine::init(&dir.unwrap_or_else(|| PathBuf::from(".")))?;
            let checked = check(&evelyn::toolchain::Tools::user()?)?;
            print(&serde_json::json!({ "repository": repository, "checked": checked }))
        }
        Command::Check => print(&check(&evelyn::toolchain::Tools::user()?)?),
        Command::Extract { firmware } => {
            let root = evelyn::engine::profile::find(Path::new("."))?;
            let mut extracted = Vec::new();
            for (side, name) in [(FirmwareArg::Rom, "rom"), (FirmwareArg::Stock, "stock")] {
                if firmware.is_none_or(|firmware| firmware == side) {
                    let dir = root.join("firmware").join(name);
                    extracted.extend(evelyn::engine::extract_in_place(
                        &dir,
                        side == FirmwareArg::Stock,
                    )?);
                }
            }
            print(&extracted)
        }
        Command::Build {
            inputs,
            output,
            work_dir,
            output_dir,
            patches,
            dry_run,
            debug,
            selinux_permissive,
            no_dexpreopt,
        } => {
            let root = evelyn::engine::profile::find(Path::new("."))?;
            print(&evelyn::engine::build(&evelyn::engine::Build {
                root: &root,
                rom: inputs.rom.as_deref().map(evelyn::engine::Given::parse),
                stock: inputs.stock.as_deref().map(evelyn::engine::Given::parse),
                output: output.map(Output::from),
                work_dir: &work_dir.unwrap_or_else(|| root.join(".evelyn")),
                output_dir: output_dir.as_deref(),
                extra_patches: &patches,
                dry_run,
                debug,
                selinux_permissive,
                dexpreopt: !no_dexpreopt,
                tools: &evelyn::toolchain::Tools::user()?,
            })?)
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(io::stderr)
        .init();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let mut messages = messages(&error).into_iter();
            eprintln!("error: {}", messages.next().unwrap_or_default());
            for cause in messages {
                eprintln!("  caused by: {cause}");
            }
            ExitCode::FAILURE
        }
    }
}
