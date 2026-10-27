//! Recipe steps applied to partition targets and decoded packages: bindings and
//! conditions read from the composed firmware, operations guarded by the content
//! checks the recipe declares, plus capture of new operations and preview of their effect.

mod apktool;
mod binary;
pub mod capture;
mod compose;
mod layout;
mod operations;
mod package;
pub mod preview;
mod scope;
mod target;
mod text;
mod xml;

pub use compose::{Absent, Composition, Context, Report, Skipped, Sources};
pub use target::{Error, Images, Target};
