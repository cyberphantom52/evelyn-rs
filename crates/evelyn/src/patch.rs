//! Recipe steps applied to partition targets and decoded packages: bindings and
//! conditions read from the composed firmware, and each operation a function
//! over the build's files or an open package's.

mod align;
mod apktool;
mod binary;
mod compose;
mod defaults;
mod eval;
mod files;
mod layout;
mod operations;
mod package;
mod sepolicy;
mod target;
mod text;
mod xml;

pub use compose::{Composition, Context, Skipped, Sources};
pub use target::{Error, Images, Target};
