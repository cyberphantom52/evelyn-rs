# Evelyn

Android ROM porting tool. A **port** combines a ROM's system side with a
device's own vendor side, applies **recipes** to them, and builds partition
images, a `super.img`, or a recovery-flashable ZIP.

Source images (EROFS or ext4, raw or sparse) are read in place, never
extracted. Only the files a recipe changes are written out; everything else
streams straight from the source into the new image.

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
target/release/evelyn build devices/pong/OxygenOS/17 --rom <rom images> --stock <stock images> \
    --output super --output-dir out

# A debug build: adb as root, boot logs in /data/local/tmp/logs
target/release/evelyn build <port> --rom … --stock … --output-dir out --recipe tools/debug_build/debug.ron
```

`--work-dir` keeps scans and the composed trees between builds, and
`--output-dir` is where the results go. `evelyn inspect` checks images,
ports and recipes, and `evelyn patch` turns a hand-edited file into a new
recipe.

A port takes the device's own partitions (`vendor`, `odm`, `vendor_dlkm`,
`odm_dlkm`) from the stock firmware, checked against the build fingerprint
`device.toml` pins, and `system`, `system_ext`, `product` and `system_dlkm` from the
ROM. Other ROM partitions are taken only when the port lists them. Recipes are found
by directory: `roms/common`, then `roms/<rom>/<version>/common`, then the port
directory.

## Layout

- `devices/<device>/device.toml`: the device: product IDs, the stock build it is pinned to, and its `super` layout
- `devices/<device>/<rom>/<version>/`: a port (`port.toml`) and its recipes (`*.ron`)
- `roms/`: recipes shared by every device on a ROM
- `crates/`: the Rust workspace
  - `android-fs`: EROFS and ext4 images: read, scan, write
  - `android-property`: `.prop` files
  - `smali`: smali parser and method edits
  - `patch`: the patch engine
  - `recipe`, `model`: device, port and recipe formats
  - `engine`: the build pipeline
  - `toolchain`: the pinned host tools
  - `cli`: the `evelyn` binary
- `tools/`: the host tool manager, the ZIP packaging template and the debug recipe

The shell implementation (`build.sh`, `tools/*.sh`, the `patch.sh` files) is
still in the tree until the remaining ports are moved over.

## TODO

- [ ] Read OTA packages directly (`payload.bin` partitions in place, `.dat` images) instead of pre-extracted images in `cache/`
- [ ] Flash-test the ZIP output
- [ ] Port the remaining ROMs and devices to recipes: NothingOS 13, OxygenOS 15/16, Pixel 12–14, raphaeldyn
- [ ] Remove the shell implementation once nothing uses it
- [ ] Build super images, align and sign APKs in-process instead of with `lpmake`, `zipalign` and `apksigner`, after testing edge cases against those tools
- [ ] Sparse image output
- [ ] Port system-as-root ROMs to devices with the older `/system` layout (and the reverse), as the old shell's `merge_partition` did
- [ ] Patch boot images (`boot`, `vendor_boot`) in Evelyn instead of beforehand, and check the first-stage fstab in `vendor_boot` as well as `vendor/etc`
- [ ] Download assets such as the VNDK APEX from a separate assets repository instead of keeping them in git
- [ ] Instruction-level smali patches (replace a call or constant in every class): make the smali tree lossless, print it back, and edit it as a tree with visitors instead of splicing text, with the method-level edits rebuilt on top

## Authors

- [@Cyber-Phantom](https://github.com/Cyber-Phantom)
