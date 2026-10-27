//! Smali parsing with source spans.
//!
//! [`parse`] turns a `.smali` file into a [`Class`] whose nodes borrow from the
//! source.

pub mod ast;
mod parser;

pub use ast::{BlockKind, Class, Field, Instruction, Method, Operand, Statement};
pub use parser::{Error as ParseError, parse};
