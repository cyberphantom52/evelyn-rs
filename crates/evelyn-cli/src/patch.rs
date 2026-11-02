use crate::{Error, print};
use clap::{Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Copy a file, as the last build of the current repository composed it, out
    /// for editing.
    Prepare {
        #[command(flatten)]
        inputs: crate::InputArgs,
        /// The file as the device sees it, such as /vendor/build.prop.
        path: String,
        /// The build workspace to read [default: as for `evelyn build`].
        #[arg(long)]
        work_dir: Option<PathBuf>,
        /// Where to put the editable copy.
        #[arg(long, default_value = "dev")]
        into: PathBuf,
    },
    /// Capture the edited copy as a new patch directory.
    Capture {
        workspace: PathBuf,
        /// The new patch directory, such as patches/<topic>/<name>.
        output: PathBuf,
        #[arg(long)]
        name: String,
        #[arg(long, value_enum, default_value = "text")]
        kind: CaptureArg,
    },
    /// Compare two trees' content, Android metadata and hardlinks.
    Preview { before: PathBuf, after: PathBuf },
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum CaptureArg {
    /// Unified diff of UTF-8 text; replay edits the file and all its hardlinks.
    Text,
    /// Same-length byte edits; replay edits the file and all its hardlinks.
    Binary,
    /// Whole-file replacement; replay installs over this path alone, with Android's defaults.
    Replace,
}

impl From<CaptureArg> for engine::CaptureKind {
    fn from(value: CaptureArg) -> Self {
        match value {
            CaptureArg::Text => Self::Text,
            CaptureArg::Binary => Self::Binary,
            CaptureArg::Replace => Self::Replace,
        }
    }
}

pub(crate) fn run(command: Command) -> Result<(), Error> {
    match command {
        Command::Prepare {
            inputs,
            path,
            work_dir,
            into,
        } => {
            let plan = inputs.plan(Some(engine::Output::Partitions), &[])?;
            print(&engine::prepare_file(engine::PrepareRequest {
                tools: &toolchain::Tools::user()?,
                work_dir: &crate::resolve_work_dir(work_dir, &plan),
                plan: &plan,
                path: path.try_into()?,
                into: &into,
            })?)
        }
        Command::Capture {
            workspace,
            output,
            name,
            kind,
        } => print(&engine::capture_file(engine::CaptureRequest {
            workspace: &workspace,
            output: &output,
            name: name.try_into()?,
            kind: kind.into(),
        })?),
        Command::Preview { before, after } => print(&engine::preview_trees(&before, &after)?),
    }
}
