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

/// Rebuild a decoded package and zipalign it to `output`. APKs are then signed
/// with Evelyn's key in `tools/keys`; JARs are not verified and stay unsigned.
pub fn build(
    tools: &Tools,
    decoded: &Path,
    output: &Path,
    sign: bool,
) -> Result<(), toolchain::Error> {
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
    toolchain::run(
        tools
            .command("zipalign")?
            .args(["-P", "16", "-f", "4"])
            .arg(&unaligned)
            .arg(if sign { &aligned } else { output }),
    )?;
    let _ = std::fs::remove_file(unaligned);
    if sign {
        let keys = tools.file("keys")?;
        toolchain::run(
            tools
                .jar("apksigner.jar")?
                .args(["sign", "--v4-signing-enabled", "false"])
                .arg("--ks")
                .arg(keys.join("evelyn.p12"))
                .args(["--ks-key-alias", "evelyn", "--ks-pass"])
                .arg(format!("file:{}", keys.join("password").display()))
                .arg("--in")
                .arg(&aligned)
                .arg("--out")
                .arg(output),
        )?;
        let _ = std::fs::remove_file(aligned);
    }
    Ok(())
}
