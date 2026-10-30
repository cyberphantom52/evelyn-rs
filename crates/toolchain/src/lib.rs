//! Pinned host tools installed by `tools/manager.py` into `tools/bin` and `tools/scripts`.

use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
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

    /// A native tool from `bin/`, with the bundled `bin/lib64` on its library path.
    pub fn command(&self, name: &str) -> Result<Command, Error> {
        let path = self.installed(Path::new("bin").join(name))?;
        let mut command = Command::new(path);
        command.env("LD_LIBRARY_PATH", self.root.join("bin/lib64"));
        Ok(command)
    }

    /// A Java tool from `scripts/`, run with the host `java`.
    pub fn jar(&self, name: &str) -> Result<Command, Error> {
        let path = self.installed(Path::new("scripts").join(name))?;
        let mut command = Command::new("java");
        command.arg("-jar").arg(path);
        Ok(command)
    }

    /// Any other file or directory under the tools root, such as `keys`.
    pub fn file(&self, relative: &str) -> Result<PathBuf, Error> {
        let path = self.root.join(relative);
        if !path.exists() {
            return Err(Error::Missing(path));
        }
        Ok(path)
    }

    fn installed(&self, relative: PathBuf) -> Result<PathBuf, Error> {
        let path = self.root.join(relative);
        if !path.is_file() {
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

/// Run to completion while `feed` writes the tool's stdin, returning stdout.
/// Output is drained concurrently so a chatty tool cannot block the writer.
pub fn run_with_input<E: From<Error>>(
    command: &mut Command,
    feed: impl FnOnce(&mut dyn Write) -> Result<(), E>,
) -> Result<Vec<u8>, E> {
    let tool = command_name(command);
    tracing::debug!(command = ?command, "running with input");
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| Error::Spawn {
            tool: tool.clone(),
            source,
        })?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
    let (fed, out, err) = std::thread::scope(|scope| {
        let out = scope.spawn(move || drain(stdout));
        let err = scope.spawn(move || drain(stderr));
        let fed = feed(&mut stdin);
        drop(stdin);
        (
            fed,
            out.join().unwrap_or_default(),
            err.join().unwrap_or_default(),
        )
    });
    let status = child.wait().map_err(|source| Error::Spawn {
        tool: tool.clone(),
        source,
    })?;
    if !status.success() {
        let mut text = String::from_utf8_lossy(&out).into_owned();
        text.push_str(&String::from_utf8_lossy(&err));
        return Err(Error::Failed {
            tool,
            status,
            output: text,
        }
        .into());
    }
    fed?;
    Ok(out)
}

fn drain(pipe: Option<impl Read>) -> Vec<u8> {
    let mut bytes = Vec::new();
    if let Some(mut pipe) = pipe {
        let _ = pipe.read_to_end(&mut bytes);
    }
    bytes
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
