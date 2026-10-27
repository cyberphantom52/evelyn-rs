//! What a recipe needs before it can apply: the assets it reads from its directory
//! and the firmware partitions it reads, gathered by walking its primitive tree.

use model::{DevicePath, Name, RelativePath, Sha256Digest};
use recipe::{
    Error, Firmware,
    tree::{Expr, Stmt, Transform},
    visit::Visitor,
};

/// Asset files (with the SHA-256 the recipe pins, if any) and asset directories.
#[derive(Default)]
pub(crate) struct Assets {
    pub files: Vec<(RelativePath, Option<Sha256Digest>)>,
    pub trees: Vec<RelativePath>,
}

impl Visitor for Assets {
    fn stmt(&mut self, stmt: &Stmt, _bound: &[&Name]) -> Result<(), Error> {
        match stmt {
            Stmt::Write { asset, sha256, .. } => self.files.push((asset.clone(), sha256.clone())),
            Stmt::WriteTree { assets, .. } => self.trees.push(assets.clone()),
            Stmt::Edit { transform, .. } => match transform {
                Transform::TextPatch { patch, .. } => self.files.push((patch.clone(), None)),
                Transform::ReplaceMethod { body, .. } | Transform::InsertMethod { body, .. } => {
                    self.files.push((body.clone(), None))
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
