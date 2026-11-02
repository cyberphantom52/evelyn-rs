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
    /// Set up a device repository, which ports one ROM to one device, and check
    /// what builds need, as `check` does.
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
        /// An extra patch directory, applied after the port's.
        #[arg(long = "patch")]
        patches: Vec<PathBuf>,
        /// Apply every recipe to the firmware but write no images, and print the
        /// plan with what applied.
        #[arg(long)]
        dry_run: bool,
    },
    /// Replace the OTA package in firmware/rom or firmware/stock, or both, by its
    /// images, so builds read them in place instead of extracting the package
    /// each time. Images an earlier extraction left there are replaced.
    Extract {
        #[arg(value_enum)]
        firmware: Option<FirmwareArg>,
    },
    /// Download every file builds need that Evelyn lacks, check those it has,
    /// generate the signing key if there is none, and check for Java.
    Check,
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
    Firmware(#[from] android_firmware::Error),
    #[error(transparent)]
    Recipe(#[from] recipe::Error),
    #[error(transparent)]
    Engine(#[from] engine::Error),
    #[error(transparent)]
    Profile(#[from] engine::profile::Error),
    #[error(transparent)]
    Image(#[from] android_fs::Error),
    #[error(transparent)]
    Toolchain(#[from] toolchain::Error),
    #[error("Java is needed for apktool and apksigner: {0}")]
    Java(String),
    #[error("{0} of what builds need could not be set up")]
    Unchecked(usize),
    #[error("cannot write JSON result: {0}")]
    Json(#[from] serde_json::Error),
    #[error("cannot write output: {0}")]
    Output(#[from] io::Error),
}

/// What builds need beyond Evelyn, each with its state: Java, the pinned files
/// and the signing key, printed whether or not some fail.
fn check(tools: &toolchain::Tools) -> Result<serde_json::Value, Error> {
    let java = match std::process::Command::new("java").arg("-version").output() {
        Ok(output) if output.status.success() => Ok(String::from_utf8_lossy(&output.stderr)
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned()),
        Ok(output) => Err(String::from_utf8_lossy(&output.stderr).into_owned()),
        Err(error) => Err(error.to_string()),
    };
    let mut failed = 0;
    let mut checked = vec![match java {
        Ok(version) => {
            serde_json::json!({ "name": "java", "state": "present", "version": version })
        }
        Err(error) => {
            failed += 1;
            serde_json::json!({ "name": "java", "state": "failed", "error": Error::Java(error).to_string() })
        }
    }];
    for status in tools.check() {
        let (state, error) = match status.state {
            Ok(toolchain::State::Present) => ("present", None),
            Ok(toolchain::State::New) => ("new", None),
            Err(error) => {
                failed += 1;
                ("failed", Some(error.to_string()))
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
        Command::Init { dir, device, rom } => {
            let repository = engine::init(
                &dir.unwrap_or_else(|| PathBuf::from(".")),
                &device.try_into()?,
                &rom.try_into()?,
            )?;
            let checked = check(&toolchain::Tools::user()?)?;
            print(&serde_json::json!({ "repository": repository, "checked": checked }))
        }
        Command::Check => print(&check(&toolchain::Tools::user()?)?),
        Command::Extract { firmware } => {
            let root = engine::profile::find(Path::new("."))?;
            let mut extracted = Vec::new();
            for (side, name) in [(FirmwareArg::Rom, "rom"), (FirmwareArg::Stock, "stock")] {
                if firmware.is_none_or(|firmware| firmware == side) {
                    let dir = root.join("firmware").join(name);
                    extracted.extend(engine::extract_in_place(&dir, side == FirmwareArg::Stock)?);
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
        } => {
            let plan = inputs.plan(output.map(Output::from), &patches)?;
            let output_dir = output_dir.unwrap_or_else(|| {
                plan.profile
                    .root
                    .join("out")
                    .join(plan.profile.name.as_ref())
            });
            let built = engine::build(&engine::BuildRequest {
                tools: &toolchain::Tools::user()?,
                work_dir: &resolve_work_dir(work_dir, &plan),
                output_dir: &output_dir,
                plan: &plan,
                dry_run,
            })?;
            match dry_run {
                true => print(&serde_json::json!({ "plan": plan, "applied": built })),
                false => print(&built),
            }
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
