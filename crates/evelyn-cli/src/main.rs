mod patch;

use android_fs::Artifact;
use clap::{Parser, Subcommand, ValueEnum};
use engine::Output;
use engine::Profile;
use recipe::Recipe;
use serde::Serialize;
use std::{
    io::{self, Write},
    path::PathBuf,
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
    /// Build a port into partition images, a super image, or a recovery-flashable ZIP.
    Build {
        /// The port directory, ports/<name> in a device repository.
        port: PathBuf,
        #[command(flatten)]
        firmware: FirmwareArgs,
        #[arg(long, value_enum, default_value = "partitions")]
        output: OutputArg,
        /// Reusable scan and composition workspace [default: .evelyn in the repository].
        #[arg(long)]
        work_dir: Option<PathBuf>,
        /// Where the built images or ZIP are written.
        #[arg(long)]
        output_dir: PathBuf,
        /// Directory populated by tools/manager.py.
        #[arg(long, default_value = "tools")]
        tools: PathBuf,
        /// An extra patch directory, applied after the port's.
        #[arg(long = "patch")]
        patches: Vec<PathBuf>,
    },
    /// Author and replay recipes against extracted partition trees.
    Patch {
        #[command(subcommand)]
        action: patch::Command,
    },
    /// Inspect an image, profile or recipe.
    Inspect {
        #[command(subcommand)]
        item: Inspect,
    },
    /// Resolve a port's partitions, sources and recipe order against firmware.
    Plan {
        /// The port directory, ports/<name> in a device repository.
        port: PathBuf,
        #[command(flatten)]
        firmware: FirmwareArgs,
        #[arg(long, value_enum, default_value = "partitions")]
        output: OutputArg,
    },
}

#[derive(Subcommand)]
enum Inspect {
    /// Hash an image and identify its format.
    Image { path: PathBuf },
    /// Parse and validate a port and its repository, and list the patches it applies.
    Profile { port: PathBuf },
    /// Parse and validate a RON recipe.
    Recipe { path: PathBuf },
    /// List the properties in an image's *.prop files, read without extracting it.
    Props {
        image: PathBuf,
        /// Only this key, with the file each definition comes from.
        key: Option<String>,
    },
}

/// The firmware a port is built from, each a directory of `<partition>.img` images.
#[derive(clap::Args)]
pub(crate) struct FirmwareArgs {
    /// The ROM being ported, which supplies the system side.
    #[arg(long)]
    rom: PathBuf,
    /// The device's own firmware, which supplies the vendor side.
    #[arg(long)]
    stock: PathBuf,
}

impl FirmwareArgs {
    pub(crate) fn firmware(&self) -> engine::FirmwareDirs<'_> {
        engine::FirmwareDirs {
            rom: &self.rom,
            stock: &self.stock,
        }
    }
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
    Firmware(#[from] android_firmware::Error),
    #[error(transparent)]
    Recipe(#[from] recipe::Error),
    #[error(transparent)]
    Engine(#[from] engine::Error),
    #[error(transparent)]
    Profile(#[from] engine::profile::Error),
    #[error(transparent)]
    Image(#[from] android_fs::Error),
    #[error("cannot write JSON result: {0}")]
    Json(#[from] serde_json::Error),
    #[error("cannot write output: {0}")]
    Output(#[from] io::Error),
}

/// `--work-dir` as given, else `.evelyn` in the port's repository.
fn resolve_work_dir(flag: Option<PathBuf>, plan: &engine::Plan) -> PathBuf {
    flag.unwrap_or_else(|| plan.profile.root.join(".evelyn"))
}

fn print(value: &impl Serialize) -> Result<(), Error> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, value)?;
    writeln!(stdout)?;
    Ok(())
}

fn run(cli: Cli) -> Result<(), Error> {
    match cli.command {
        Command::Build {
            port,
            firmware,
            output,
            work_dir,
            output_dir,
            tools,
            patches,
        } => {
            let plan = engine::Plan::load(&port, output.into(), &firmware.firmware(), &patches)?;
            print(&engine::build(&engine::BuildRequest {
                tools: &toolchain::Tools::new(tools),
                work_dir: &resolve_work_dir(work_dir, &plan),
                output_dir: &output_dir,
                plan: &plan,
            })?)
        }
        Command::Patch { action } => patch::run(action),
        Command::Inspect { item } => match item {
            Inspect::Image { path } => print(&Artifact::inspect(&path)?),
            Inspect::Profile { port } => {
                let profile = Profile::load(&port)?;
                print(&serde_json::json!({ "profile": profile, "patches": profile.patches() }))
            }
            Inspect::Recipe { path } => print(&Recipe::load(&path)?),
            Inspect::Props { image, key } => {
                let source = android_fs::Image::detect(&image)?;
                let mut found = Vec::new();
                for (file, data) in source.read_files(android_property::is_property_file)? {
                    let text = String::from_utf8_lossy(&data);
                    for entry in android_property::entries(&text) {
                        if key.as_deref().is_none_or(|key| key == entry.key) {
                            found.push(serde_json::json!({
                                "file": file, "key": entry.key, "value": entry.value,
                            }));
                        }
                    }
                }
                print(&found)
            }
        },
        Command::Plan {
            port,
            firmware,
            output,
        } => print(&engine::Plan::load(
            &port,
            output.into(),
            &firmware.firmware(),
            &[],
        )?),
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
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
