//! Recipe steps applied to partition targets and decoded packages: bindings and
//! conditions read from the composed firmware, operations guarded by the content
//! checks the recipe declares.

mod align;
mod apktool;
mod binary;
mod compose;
mod defaults;
mod layout;
mod operations;
mod package;
mod scope;
mod sepolicy;
mod target;
mod text;
mod xml;

pub use compose::{Composition, Context, Skipped, Sources};
pub use target::{Error, Images, Target};
