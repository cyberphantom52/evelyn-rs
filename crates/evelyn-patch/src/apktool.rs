//! Decode and rebuild APK/JAR packages with apktool.

use std::path::Path;
use toolchain::Tools;

/// Decode `input` into a new `output` directory. Without resources, only
/// smali and raw files are editable and resources are copied through unchanged.
pub fn decode(
    tools: &Tools,
    input: &Path,
    output: &Path,
    resources: bool,
) -> Result<(), toolchain::Error> {
    let mut command = tools.jar("apktool.jar")?;
    command.arg("decode").arg("--force");
    if !resources {
        command.arg("--no-res");
    }
    command.arg("--output").arg(output).arg(input);
    toolchain::run(&mut command)?;
    Ok(())
}

/// Rebuild a decoded package and align it to `output`. APKs are then signed with
/// Evelyn's key; JARs are not verified and stay unsigned.
pub fn build(tools: &Tools, decoded: &Path, output: &Path, sign: bool) -> Result<(), crate::Error> {
    let unaligned = output.with_extension("unaligned");
    let aligned = output.with_extension("aligned");
    toolchain::run(
        tools
            .jar("apktool.jar")?
            .arg("build")
            .arg("--output")
            .arg(&unaligned)
            .arg(decoded),
    )?;
    crate::align::align(&unaligned, if sign { &aligned } else { output })?;
    let _ = std::fs::remove_file(unaligned);
    if sign {
        let keys = tools.keys()?;
        toolchain::run(
            tools
                .jar("apksigner.jar")?
                .args(["sign", "--v4-signing-enabled", "false"])
                .arg("--key")
                .arg(&keys.key)
                .arg("--cert")
                .arg(&keys.certificate)
                .arg("--in")
                .arg(&aligned)
                .arg("--out")
                .arg(output),
        )?;
        let _ = std::fs::remove_file(aligned);
    }
    Ok(())
}
