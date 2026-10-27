//! What Android gives a file it has no other metadata for: the owner, group, mode
//! and capabilities of `fs_config` ([`FsConfig`]) and the SELinux label of the
//! `file_contexts` policy ([`FileContexts`]), with the rules Android itself uses.

mod file_contexts;
mod fs_config;

pub use file_contexts::FileContexts;
pub use fs_config::{FsConfig, Ownership, capability_xattr};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("fs_config entry {0} is truncated or has no terminating NUL")]
    FsConfig(usize),
    #[error("file_contexts line {line}: {message}")]
    FileContexts { line: usize, message: String },
}

/// The kind of file a `file_contexts` spec applies to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    File,
    Directory,
    Symlink,
    CharDevice,
    BlockDevice,
    Socket,
    Pipe,
}
