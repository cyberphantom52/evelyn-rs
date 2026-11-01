//! What a recipe needs before it can apply: the assets it reads from its directory
//! and the firmware partitions it reads, gathered by walking its primitive tree.

use android_firmware::{DevicePath, Firmware, RelativePath};
use recipe::{
    Error,
    tree::{Expr, MethodEdit, Source, Stmt, Transform},
    visit::Visitor,
};

/// The asset files and directories a recipe reads.
#[derive(Default)]
pub(crate) struct Assets(pub Vec<RelativePath>);

impl Visitor for Assets {
    fn expr(&mut self, expr: &Expr, _bound: &[&recipe::tree::Binding]) -> Result<(), Error> {
        // A computed path is looked up when its step runs.
        if let Expr::Select { pattern, scope } = expr
            && scope.inside == Some(Source::Assets)
            && let Some(pattern) = pattern.literal_str()
        {
            self.0.push(RelativePath::try_from(pattern.to_owned())?);
        }
        Ok(())
    }

    fn stmt(&mut self, stmt: &Stmt, _bound: &[&recipe::tree::Binding]) -> Result<(), Error> {
        if let Stmt::Edit { transform, .. } = stmt {
            let assets: Vec<&Expr> = match transform {
                Transform::ApplyDiff(patch) => vec![patch],
                Transform::AddPolicy { policies, .. } => policies.iter().collect(),
                Transform::Smali {
                    edit: MethodEdit::Replace(body) | MethodEdit::Insert(body),
                    ..
                } => vec![body],
                _ => Vec::new(),
            };
            // One naming a bound value is looked up when its step runs.
            for asset in assets.into_iter().filter_map(Expr::literal_str) {
                self.0.push(RelativePath::try_from(asset.to_owned())?);
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
