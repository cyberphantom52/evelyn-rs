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
ports/<name>/port.toml one ROM version: the ROM builds it is for, what it takes from the ROM, and the patches it applies, in order
patches/<topic>/<name>/patch.ron  one patch, with its assets beside it
firmware/stock/        the device's stock firmware: a full OTA package or <partition>.img images
firmware/rom/          the ROM being ported, likewise
out/<port>/            what a build writes
.evelyn/               scans kept between builds, and each build's working space
```

A build takes the port the ROM matches: each port says which builds it is for
as globs over properties the ROM sets, such as `"ro.build.version.release" = "17"`
under `[rom]`. Porting a new OTA is replacing what `firmware/rom` holds.

A build reads `firmware/rom` and `firmware/stock` as they are: images in place,
or an OTA package, of which it extracts only the partitions it reads, for that
build alone: the ROM's system side, whose properties pick the port, then what
the port takes and its patches read, and the device's own partitions of the
stock firmware. `evelyn extract` replaces a package by its images for good, so
builds stop extracting it; a directory holding both a package and images is
refused.

A patch applies only where a port lists it, so two versions share a patch by
listing it, and a new version starts with nothing until its patches are
carried over and checked against it.

Every build first applies what Evelyn does for any port: the VNDK APEX a
vendor older than Android 15 needs, when the ROM lacks it.

A port takes the device's own partitions (`vendor`, `odm`, `vendor_dlkm`,
`odm_dlkm`) from the stock firmware, checked against the build fingerprint
`evelyn.toml` pins, and `system`, `system_ext`, `product` and `system_dlkm` from the
ROM. Other ROM partitions are taken only when the port lists them.

## Requirements

- Rust (the toolchain is pinned in `rust-toolchain.toml`)
- A C compiler, which builds liblz4 for EROFS compression
- Java, for apktool and apksigner

What else a build needs Evelyn keeps in its data directory
(`~/Library/Application Support/evelyn` on macOS, `~/.local/share/evelyn` on
Linux): apktool, apksigner, VNDK APEXes and Android's SELinux macros, downloaded the first time one is
needed and checked against the SHA-256 Evelyn pins, and the key it signs APKs
with, generated the first time. `evelyn check` sets all of it up at once,
checks what is there and reports what is missing; `evelyn init` runs it too.

## Usage

```sh
cargo build --release

# A device repository: fill in the stock fingerprint and product IDs in
# evelyn.toml, add ports/<name>/port.toml and patches/, and put the firmware in
# firmware/stock and firmware/rom.
evelyn init pong-oxygenos --device pong --rom OxygenOS

# Inside the repository: a super image, or partition images for a device without
# a super layout, in out/<port>.
evelyn build
```

Everything a build reads has a default, and each can be given instead:
`--rom` and `--stock` (an OTA package, local or an `http(s)://` URL, or a
directory of images), `--output partitions|super|zip`, `--output-dir` and `--work-dir`. A ZIP also
flashes the stock OTA's partitions outside `super`, or the images in
`firmware/stock/firmware/`. Of a package at a URL, only the partitions extracted
are downloaded, so a CI job can build from links alone:
`evelyn build --rom <url> --stock <url>`. `--patch <directory>` applies an extra patch for one
build, such as a debug build's. `--dry-run` applies every patch to the firmware
but writes no images, and prints the plan with what applied. `evelyn inspect`
checks images, ports and recipes, and `evelyn patch` turns a hand-edited file
into a new patch.

## Layout

- `crates/`: the Rust workspace
  - `android-firmware`: partition names and paths, mount points, Treble sides, the `super` layout and Android file metadata
  - `android-fs`: EROFS and ext4 images (raw or sparse) read in place, scanned into partition trees and built back, alone or straight into a `super` image
  - `android-property`: `.prop` files
  - `smali`: smali parser and method edits
  - `evelyn-recipe`: the recipe language
  - `evelyn-patch`: applies recipes to partition trees
  - `evelyn-engine`: device repositories, planning and the build pipeline
  - `toolchain`: the files Evelyn downloads, pinned by SHA-256, and its signing key
  - `evelyn-cli`: the `evelyn` binary
- `assets/`: files compiled into Evelyn: the recipes every build applies and the ZIP installer

## TODO

- [ ] Read `.dat` (sdat) firmware images
- [ ] Flash-test the ZIP output
- [ ] Sparse image output
- [ ] Port system-as-root ROMs to devices with the older `/system` layout (and the reverse)
- [ ] Patch boot images (`boot`, `vendor_boot`) in Evelyn instead of beforehand, and check the first-stage fstab in `vendor_boot` as well as `vendor/etc`
- [ ] Instruction-level smali patches (replace a call or constant in every class): make the smali tree lossless, print it back, and edit it as a tree with visitors instead of splicing text, with the method-level edits rebuilt on top

## Authors

- [@Cyber-Phantom](https://github.com/Cyber-Phantom)
