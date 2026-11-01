mod patch;

use android_firmware::Name;
use android_fs::Artifact;
use clap::{Parser, Subcommand, ValueEnum};
use engine::Output;
use engine::Profile;
use recipe::Recipe;
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
    /// Set up a device repository, which ports one ROM to one device.
    Init {
        /// The repository's directory [default: the current directory].
        dir: Option<PathBuf>,
        /// The device, as the repository names it.
        #[arg(long)]
        device: String,
        /// The ROM it ports to the device.
        #[arg(long)]
        rom: String,
    },
    /// Build the device repository the current directory is in into partition
    /// images, a super image, or a recovery-flashable ZIP.
    Build {
        #[command(flatten)]
        inputs: InputArgs,
        /// [default: super with a super layout in evelyn.toml, else partitions]
        #[arg(long, value_enum)]
        output: Option<OutputArg>,
        /// Reusable scan and composition workspace [default: .evelyn in the repository].
        #[arg(long)]
        work_dir: Option<PathBuf>,
        /// Where the built images or ZIP are written [default: out/<port> in the repository].
        #[arg(long)]
        output_dir: Option<PathBuf>,
        /// Directory populated by tools/manager.py [default: tools in the Evelyn
        /// checkout this was built from].
        #[arg(long)]
        tools: Option<PathBuf>,
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
    /// Resolve the port, partitions, sources and recipe order a build would use.
    Plan {
        #[command(flatten)]
        inputs: InputArgs,
        #[arg(long, value_enum)]
        output: Option<OutputArg>,
    },
}

#[derive(Subcommand)]
enum Inspect {
    /// Hash an image and identify its format.
    Image { path: PathBuf },
    /// Parse and validate the current repository's ports, or one of them, and
    /// list the patches each applies.
    Profile { port: Option<String> },
    /// Parse and validate a RON recipe.
    Recipe { path: PathBuf },
    /// List the properties in an image's *.prop files, read without extracting it.
    Props {
        image: PathBuf,
        /// Only this key, with the file each definition comes from.
        key: Option<String>,
    },
}

/// What a build is of, where it is not the repository's default.
#[derive(clap::Args)]
pub(crate) struct InputArgs {
    /// The ROM being ported, which supplies the system side: a directory of
    /// <partition>.img images or an OTA package [default: firmware/rom].
    #[arg(long)]
    rom: Option<PathBuf>,
    /// The device's own firmware, which supplies the vendor side, likewise
    /// [default: firmware/stock].
    #[arg(long)]
    stock: Option<PathBuf>,
    /// The port to build [default: the one the ROM matches].
    #[arg(long)]
    port: Option<String>,
}

impl InputArgs {
    /// Plan a build of the repository the current directory is in.
    pub(crate) fn plan(
        &self,
        output: Option<Output>,
        extra: &[PathBuf],
    ) -> Result<engine::Plan, Error> {
        let root = engine::profile::find(Path::new("."))?;
        let port = self.port.clone().map(Name::try_from).transpose()?;
        let inputs = engine::Inputs {
            rom: self.rom.as_deref(),
            stock: self.stock.as_deref(),
            port: port.as_ref(),
        };
        Ok(engine::Plan::load(&root, &inputs, output, extra)?)
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

/// `--tools` as given, else `tools` in the Evelyn checkout this was built from,
/// where tools/manager.py installs them.
pub(crate) fn tools_or_default(tools: Option<PathBuf>) -> toolchain::Tools {
    toolchain::Tools::new(
        tools.unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools")),
    )
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
        Command::Init { dir, device, rom } => print(&engine::init(
            &dir.unwrap_or_else(|| PathBuf::from(".")),
            &device.try_into()?,
            &rom.try_into()?,
        )?),
        Command::Build {
            inputs,
            output,
            work_dir,
            output_dir,
            tools,
            patches,
        } => {
            let plan = inputs.plan(output.map(Output::from), &patches)?;
            let output_dir = output_dir.unwrap_or_else(|| {
                plan.profile
                    .root
                    .join("out")
                    .join(plan.profile.name.as_ref())
            });
            print(&engine::build(&engine::BuildRequest {
                tools: &tools_or_default(tools),
                work_dir: &resolve_work_dir(work_dir, &plan),
                output_dir: &output_dir,
                plan: &plan,
            })?)
        }
        Command::Patch { action } => patch::run(action),
        Command::Inspect { item } => match item {
            Inspect::Image { path } => print(&Artifact::inspect(&path)?),
            Inspect::Profile { port } => {
                let root = engine::profile::find(Path::new("."))?;
                let names = match port {
                    Some(port) => vec![Name::try_from(port)?],
                    None => engine::profile::ports(&root)?.into_keys().collect(),
                };
                let mut profiles = Vec::new();
                for name in names {
                    let profile = Profile::load(&root, &name)?;
                    profiles.push(
                        serde_json::json!({ "profile": profile, "patches": profile.patches() }),
                    );
                }
                print(&profiles)
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
        Command::Plan { inputs, output } => print(&inputs.plan(output.map(Output::from), &[])?),
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
