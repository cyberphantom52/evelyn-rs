//! Decode and rebuild the files of APK/JAR packages with apktool. Code is not
//! its business: the DEX files pass through as they are.

use crate::toolchain::Tools;
use std::path::Path;

/// Decode `input` into a new `output` directory. Without resources, only raw
/// files are editable and resources are copied through unchanged.
pub fn decode(
    tools: &Tools,
    input: &Path,
    output: &Path,
    resources: bool,
) -> Result<(), crate::toolchain::Error> {
    let mut command = tools.jar("apktool.jar")?;
    command.args(["decode", "--force", "--no-src"]);
    if !resources {
        command.arg("--no-res");
    }
    command.arg("--output").arg(output).arg(input);
    crate::toolchain::run(&mut command)?;
    Ok(())
}

/// Rebuild a decoded package to `output`, neither aligned nor signed.
pub fn build(tools: &Tools, decoded: &Path, output: &Path) -> Result<(), crate::toolchain::Error> {
    let mut command = tools.jar("apktool.jar")?;
    command
        .arg("build")
        .arg("--output")
        .arg(output)
        .arg(decoded);
    crate::toolchain::run(&mut command)?;
    Ok(())
}
