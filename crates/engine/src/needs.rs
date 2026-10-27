//! What a recipe needs before it can apply: the assets it reads from its directory
//! and the firmware partitions it reads, gathered by walking its primitive tree.

use model::{DevicePath, Name, RelativePath};
use recipe::{
    Error, Firmware,
    tree::{Expr, Stmt, Transform},
    visit::Visitor,
};

/// The asset files and directories a recipe reads.
#[derive(Default)]
pub(crate) struct Assets(pub Vec<RelativePath>);

impl Visitor for Assets {
    fn stmt(&mut self, stmt: &Stmt, _bound: &[&Name]) -> Result<(), Error> {
        match stmt {
            Stmt::Write { asset, .. } => self.0.push(asset.clone()),
            Stmt::Edit { transform, .. } => match transform {
                Transform::TextPatch { patch, .. } => self.0.push(patch.clone()),
                Transform::ReplaceMethod { body, .. } | Transform::InsertMethod { body, .. } => {
                    self.0.push(body.clone())
                }
                _ => {}
            },
            _ => {}
        }
        Ok(())
    }
}

/// Every selection that looks inside a firmware: which firmware, and the device
/// path it looks for.
#[derive(Default)]
pub(crate) struct Sources {
    pub reads: Vec<(Firmware, DevicePath)>,
}

impl Visitor for Sources {
    fn expr(&mut self, expr: &Expr, _bound: &[&Name]) -> Result<(), Error> {
        if let Expr::Select { pattern, scope } = expr
            && let Some(firmware) = scope.inside
        {
            self.reads
                .push((firmware, DevicePath::try_from(pattern.clone())?));
        }
        Ok(())
    }
}
