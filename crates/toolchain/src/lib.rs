//! Pinned host tools installed by `tools/manager.py`: the Java tools in `tools/scripts`,
//! and files such as the signing keys.

use std::{
    io,
    path::{Path, PathBuf},
    process::{Command, ExitStatus},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("{0} is not installed; run tools/manager.py")]
    Missing(PathBuf),
    #[error("cannot run {tool}: {source}")]
    Spawn {
        tool: String,
        #[source]
        source: io::Error,
    },
    #[error("{tool} failed ({status}):\n{output}")]
    Failed {
        tool: String,
        status: ExitStatus,
        output: String,
    },
}

#[derive(Clone, Debug)]
pub struct Tools {
    root: PathBuf,
}

impl Tools {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// A Java tool from `scripts/`, run with the host `java`.
    pub fn jar(&self, name: &str) -> Result<Command, Error> {
        let path = self.file(&format!("scripts/{name}"))?;
        let mut command = Command::new("java");
        command.arg("-jar").arg(path);
        Ok(command)
    }

    /// A file or directory under the tools root, such as `keys`.
    pub fn file(&self, relative: &str) -> Result<PathBuf, Error> {
        let path = self.root.join(relative);
        if !path.exists() {
            return Err(Error::Missing(path));
        }
        Ok(path)
    }
}

/// Run to completion, returning stdout. Failures carry the combined output.
pub fn run(command: &mut Command) -> Result<Vec<u8>, Error> {
    let tool = command_name(command);
    tracing::debug!(command = ?command, "running");
    let output = command.output().map_err(|source| Error::Spawn {
        tool: tool.clone(),
        source,
    })?;
    if !output.status.success() {
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        return Err(Error::Failed {
            tool,
            status: output.status,
            output: text,
        });
    }
    Ok(output.stdout)
}

fn command_name(command: &Command) -> String {
    let program = command.get_program();
    let program = if program == "java" {
        command.get_args().nth(1).unwrap_or(program)
    } else {
        program
    };
    Path::new(program)
        .file_name()
        .unwrap_or(program)
        .to_string_lossy()
        .into_owned()
}

/// The regular files under `root`, as (path relative to `root`, full path), sorted.
/// Anything other than files and directories is an error.
pub fn files(root: &Path) -> io::Result<Vec<(String, PathBuf)>> {
    let mut found = Vec::new();
    let mut pending = vec![(String::new(), root.to_path_buf())];
    while let Some((prefix, dir)) = pending.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let relative = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push((relative, entry.path()));
            } else if kind.is_file() {
                found.push((relative, entry.path()));
            } else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "{} is neither a file nor a directory",
                        entry.path().display()
                    ),
                ));
            }
        }
    }
    found.sort();
    Ok(found)
}
