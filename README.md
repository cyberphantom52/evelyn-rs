# Evelyn

Android ROM porting framework. A **port** combines a ROM's system side with a
device's own vendor side, applies **patches** to them, and builds partition
images, a `super.img`, or a recovery-flashable ZIP.

Source images (EROFS or ext4, raw or sparse) are read in place, never
extracted. Only the files a patch changes are written out; everything else
streams straight from the source into the new image.

This repository is the tool. What it ports lives in device repositories.

## Device repositories

A device repository ports one ROM to one device:

```
evelyn.toml            the ROM, and the device: product IDs, the stock build it is pinned to, its super layout
ports/<name>/port.toml one ROM version: what it takes from the ROM, and the patches it applies, in order
patches/<topic>/<name>/patch.ron  one patch, with its assets beside it
.evelyn/               scans and composed trees kept between builds
```

A patch applies only where a port lists it, so two versions share a patch by
listing it, and a new version starts with nothing until its patches are
carried over and checked against it.

A port takes the device's own partitions (`vendor`, `odm`, `vendor_dlkm`,
`odm_dlkm`) from the stock firmware, checked against the build fingerprint
`evelyn.toml` pins, and `system`, `system_ext`, `product` and `system_dlkm` from the
ROM. Other ROM partitions are taken only when the port lists them.

## Requirements

- Rust (the toolchain is pinned in `rust-toolchain.toml`)
- A C compiler, which builds liblz4 for EROFS compression
- Java, for apktool
- Python 3, to install the pinned host tools:

```sh
python3 tools/manager.py fetch
```

## Usage

```sh
cargo build --release

# Partition images, a super image, or a flashable ZIP. --rom and --stock are
# directories of <partition>.img images from the ROM and the device's firmware;
# for a ZIP, --stock/firmware/ holds the firmware images it also flashes.
evelyn build <repository>/ports/<name> --rom <rom images> --stock <stock images> \
    --tools <evelyn>/tools --output super --output-dir out

# An extra patch for this build only, such as a debug build's.
evelyn build <port> --rom … --stock … --output-dir out --patch <patch directory>
```

`--work-dir` keeps scans and the composed trees between builds, `.evelyn` in
the repository unless given, and `--output-dir` is where the results go.
`evelyn inspect` checks images, ports and recipes, and `evelyn patch` turns a
hand-edited file into a new patch.

## Layout

- `crates/`: the Rust workspace
  - `android-firmware`: partition names and paths, mount points, Treble sides, the `super` layout and Android file metadata
  - `android-fs`: EROFS and ext4 images (raw or sparse) read in place, scanned into partition trees and built back, alone or straight into a `super` image
  - `android-property`: `.prop` files
  - `smali`: smali parser and method edits
  - `evelyn-recipe`: the recipe language
  - `evelyn-patch`: applies recipes to partition trees
  - `evelyn-engine`: device repositories, planning and the build pipeline
  - `toolchain`: the pinned host tools
  - `evelyn-cli`: the `evelyn` binary
- `tools/`: the host tool manager, the ZIP packaging template and the SELinux macros

## TODO

- [ ] Read OTA packages directly (`payload.bin` partitions in place, `.dat` images) instead of pre-extracted images
- [ ] Flash-test the ZIP output
- [ ] Install the host tools in a per-user directory, so builds from a device repository need no `--tools`
- [ ] Sparse image output
- [ ] Port system-as-root ROMs to devices with the older `/system` layout (and the reverse)
- [ ] Patch boot images (`boot`, `vendor_boot`) in Evelyn instead of beforehand, and check the first-stage fstab in `vendor_boot` as well as `vendor/etc`
- [ ] Instruction-level smali patches (replace a call or constant in every class): make the smali tree lossless, print it back, and edit it as a tree with visitors instead of splicing text, with the method-level edits rebuilt on top

## Authors

- [@Cyber-Phantom](https://github.com/Cyber-Phantom)
