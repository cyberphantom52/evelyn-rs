//! What a recipe needs before it can apply: the assets it reads from its directory
//! and the firmware partitions it reads, gathered by walking its primitive tree.

use android_firmware::{DevicePath, Firmware, Name, RelativePath};
use recipe::{
    Error,
    tree::{Expr, MethodEdit, Source, Stmt, Transform},
    visit::Visitor,
};

/// The asset files and directories a recipe reads.
#[derive(Default)]
pub(crate) struct Assets(pub Vec<RelativePath>);

impl Visitor for Assets {
    fn expr(&mut self, expr: &Expr, _bound: &[&Name]) -> Result<(), Error> {
        if let Expr::Select { pattern, scope } = expr
            && scope.inside == Some(Source::Assets)
        {
            self.0.push(RelativePath::try_from(pattern.clone())?);
        }
        Ok(())
    }

    fn stmt(&mut self, stmt: &Stmt, _bound: &[&Name]) -> Result<(), Error> {
        if let Stmt::Edit { transform, .. } = stmt {
            match transform {
                Transform::TextPatch { patch, .. } => self.0.push(patch.clone()),
                Transform::Sepolicy { policies, .. } => self.0.extend(policies.iter().cloned()),
                Transform::Smali {
                    edit: MethodEdit::Replace(body) | MethodEdit::Insert(body),
                    ..
                } => self.0.push(body.clone()),
                _ => {}
            }
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
            && let Some(Source::Firmware(firmware)) = scope.inside
        {
            self.reads
                .push((firmware, DevicePath::try_from(pattern.clone())?));
        }
        Ok(())
    }
}
