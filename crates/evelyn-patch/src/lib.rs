//! Recipe steps applied to partition targets and decoded packages: bindings and
//! conditions read from the composed firmware, operations guarded by the content
//! checks the recipe declares, plus capture of new operations and preview of their effect.

mod align;
mod apktool;
mod binary;
pub mod capture;
mod compose;
mod defaults;
mod layout;
mod operations;
mod package;
pub mod preview;
mod scope;
mod target;
mod text;
mod xml;

pub use compose::{Composition, Context, Report, Skipped, Sources, Unchanged};
pub use target::{Error, Images, Target};
