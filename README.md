# Evelyn

Android ROM porting framework. A **port** combines a ROM's system side with a
device's own vendor side, applies **patches** to them, and builds partition
images, a `super.img`, or a recovery-flashable ZIP.

Source images (EROFS or ext4, raw or sparse) are read in place, never
extracted. Only the files a patch changes are written out; everything else
streams straight from the source into the new image. The ramdisks of the stock
`boot`, `init_boot` and `vendor_boot` images are patched the same way, through
[bootimg](https://github.com/cyberphantom52/bootimg).

This repository is the tool. What it ports lives in device repositories.

The [Evelyn Guide](https://cyberphantom52.github.io/evelyn-rs/) covers
installing Evelyn, writing a first port, and the full reference for
`evelyn.toml`, ports, recipes and kits. Its source is in [`book/`](book/); to
read it locally, run `mdbook serve book`.

## Requirements

- Rust, at least the `rust-version` in the root `Cargo.toml`
- A C and C++ compiler, for the dependencies that build native code: the SELinux
  policy library (C++), and the xz, zstd and `ring` libraries (C)

## Quick start

```sh
cargo install --path crates/evelyn

# A device repository: fill in the product ID and stock fingerprint in
# evelyn.toml, add ports/<name>.toml and patches/, and put the firmware in
# firmware/stock and firmware/rom.
evelyn init pong-oxygenos
cd pong-oxygenos

# Check what the build composes, then build it into out/<port>.
evelyn build --dry-run
evelyn build
```

## Layout

- `crates/`: the Rust workspace
  - `android`: what is true of Android whatever ports it: EROFS and ext4 images (raw or sparse) read in place, scanned into partition trees and built back, alone or straight into a `super` image; boot images' ramdisks, scanned and built back into their images; partition paths, mount points and Treble sides; Android file metadata and its defaults; `.prop` files
  - `smali`: smali as a lossless tree, and edits of its methods and instructions
  - `evelyn`: the `evelyn` binary, and the library it is built on:
    - `recipe`: the recipe language and its kits
    - `patch`: applies recipes to partition trees
    - `engine`: device repositories and the build pipeline
    - `toolchain`: the files Evelyn downloads, pinned by SHA-256, and its signing key
- `assets/`: files compiled into Evelyn: the recipes every build applies, the kits, the ZIP installer and the SELinux macros

## TODO

- [ ] Write a patch by editing a file: copy a file out of the last build, edit it, and turn the edit into a new patch (a text diff, same-length byte edits, or a whole-file replacement)
- [ ] Read `.dat` (sdat) firmware images
- [ ] Flash-test the ZIP output
- [ ] Sparse image output
- [ ] Port system-as-root ROMs to devices with the older `/system` layout (and the reverse)
- [ ] Take a HAL from a donor firmware: list the files it needs that the device lacks (its libraries' closure, checked against the device's vendor), and derive its SELinux policy from the donor's compiled policy, keeping the rules whose types the device's policy defines and reporting the rest
- [ ] Inventory a port's HALs: those the ROM's system side uses, against those the device's vendor declares (VINTF manifests and vendor APEXes)

## Authors

- [@Cyber-Phantom](https://github.com/Cyber-Phantom)
