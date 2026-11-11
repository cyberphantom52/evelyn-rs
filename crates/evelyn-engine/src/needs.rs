//! The firmware partitions a recipe reads, gathered by walking its primitive tree.

use android_firmware::{DevicePath, Firmware};
use recipe::{
    Error,
    tree::{Expr, Source},
    visit::Visitor,
};

/// Every selection that looks inside a firmware: which firmware, and the device
/// path it looks for.
#[derive(Default)]
pub(crate) struct Sources {
    pub reads: Vec<(Firmware, DevicePath)>,
}

impl Visitor for Sources {
    fn expr(&mut self, expr: &Expr, _bound: &[&recipe::tree::Binding]) -> Result<(), Error> {
        if let Expr::Select { pattern, scope } = expr
            && let Some(firmware) = scope.inside.and_then(Source::firmware)
            && let Some(pattern) = pattern.literal_str()
        {
            self.reads
                .push((firmware, DevicePath::try_from(pattern.to_owned())?));
        }
        Ok(())
    }
}
