//! The firmware partitions a recipe reads.

use android::firmware::{DevicePath, Firmware};
use recipe::{Expr, Location, Operation, Recipe};

/// Every path a recipe looks for in a firmware, `Rom("/…")` or `Stock("/…")`
/// written out: which firmware, and the device path. One naming a bound value is
/// not known until the recipe is applied.
pub(crate) fn reads(
    recipe: &Recipe,
) -> Result<Vec<(Firmware, DevicePath)>, android::firmware::Error> {
    let locations = recipe.steps.iter().flat_map(|step| {
        let exprs = step.bindings.values().chain(&step.when);
        exprs
            .flat_map(Expr::locations)
            .chain(step.operations.iter().flat_map(Operation::locations))
    });
    let mut reads = Vec::new();
    for location in locations {
        let firmware = match location {
            Location::Rom(_) => Firmware::Rom,
            Location::Stock(_) => Firmware::Stock,
            _ => continue,
        };
        if let Some(path) = recipe::written_out(location.path()) {
            reads.push((firmware, DevicePath::try_from(path)?));
        }
    }
    Ok(reads)
}
