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
.evelyn/               each build's working space
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

## Recipes

A patch's `patch.ron` is a recipe: steps, each a list of operations, applied in
the order their `after:` lists allow, declaration order breaking ties.

```ron
Recipe(
    name: "vndk",
    steps: [
        Step(
            id: "vndk-apex",
            let: { "vndk": Property(key: "ro.vndk.version", target: Partition("vendor")) },
            when: Not(Exists(Build("/system_ext/apex/com.android.vndk.v{vndk}.apex"))),
            operations: [
                Remove(pattern: "/system_ext/apex/com.android.vndk*.apex"),
                Install(asset: "apex/com.android.vndk.v{vndk}.apex", to: "/system_ext/apex/com.android.vndk.v{vndk}.apex"),
            ],
        ),
    ],
)
```

`let:` binds names to values read from the build so far, which any string in
the step names as `{name}`; `when:` is a condition the step needs. A value that
is not set, or a condition that does not hold, skips the step and is reported.
Paths are device paths (`/vendor/etc/fstab.qcom`), globs allowed; asset paths
are relative to the patch's directory. Beyond its syntax, nothing is checked
before a recipe is applied: a wrong path or value fails the build at its step.

The operations:

- `Copy(from:, to:, metadata: Source | Default, overwrite:, create_parent:)`
  copies from a firmware, `Rom("/…")` or `Stock("/…")`, from the build,
  `Build("/…")`, or an asset, `Asset("…")`. `Import(Rom("/…"))` is a copy to the
  same path with the firmware's metadata; `Install(asset:, to:)` a copy of an
  asset with Android's default metadata.
- `Remove(pattern:)` removes from the build; a relative pattern is tried in every
  partition that came from `origin: Rom | Stock` or is named by `partition:`.
  Matching nothing is reported, not an error.
- `SetMetadata(pattern:, uid:, gid:, mode:, selinux:)` sets what Android's
  defaults do not give.
- `SetProperty(key:, value:, target:)` sets a property where init loads it, or in
  a `Partition(…)` or a `Build(…)` file; `RemoveProperty(key:, target:)` removes
  it, and reports a key defined nowhere.
- `ApplyDiff(target:, patch:)`, `WriteBytes(target:, edits: {offset: "hex"})`,
  `EditXml(target:, edits: [SetText, SetAttribute, Remove, PutChild])` and
  `Append(target:, content:)` edit a file.
- `AddPolicy(target:, policies:)` adds TE policies to one of the vendor's CIL
  files, checked to resolve with the rest of the device's policy.
- `Package(target:, edits:)` decodes an APK or JAR and applies edits to it, with
  paths from its root, including `StubMethod`, `ReplaceMethod` and `InsertMethod`.
- `Assert(condition:, message:)` and `AssertHash(target:, sha256:)` fail the build
  when the firmware is not what the patch expects.
- `ForEach(over:, name:, operations:)` applies operations once for each string of
  a list, or each file of a selection, with `{name}` bound to it.
- `When(condition:, operations:)` applies operations if the condition holds.

Expressions select files (`Build`, `Rom`, `Stock`, `Asset`, `Package`), read them
(`Read`, `Property(key:, target:)`, `Lines`), test values (`Exists`, which also
takes a `Partition(…)`, `Not`, `All`, `Matches(text, "regex")`, `Contains(text,
"…")`) and combine them (`Union`, `Filter(over:, name:, keep:)`, `Join`).

### Kits

Operations specific to a ROM family come in kits Evelyn ships, `assets/kits/`,
which define them out of the operations above. Every recipe can call any kit's
operation as it calls a built-in one: the one parameter it must give bare, or
any by name, those with a default included.

- `oplus.ron`: `EnableFeature(Android("…"))`, `EnableFeature(Oplus("…"))` and
  `EnableFeature(App(name:, file:))` declare a feature, replacing one declared
  with that name. An Android or Oplus feature goes in its kind's file under
  `/my_product/etc/permissions`, or in the one given:
  `EnableFeature(Oplus(name: "…", file: "/…"))`. An app feature can have `args:`.
  `DisableFeature("…")` removes one from every permissions and extension XML
  that mentions it, and reports a feature declared nowhere.

## Requirements

- Rust (the toolchain is pinned in `rust-toolchain.toml`)
- A C compiler, which builds liblz4 for EROFS compression
- Java, for apktool and apksigner

What else a build needs Evelyn keeps in its data directory
(`~/Library/Application Support/evelyn` on macOS, `~/.local/share/evelyn` on
Linux): apktool, apksigner and VNDK APEXes, downloaded the first time one is
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
but writes no images, and prints what the build composed: the port, each
partition and the firmware it comes from, the patches in order, the ROM
partitions left out and the steps skipped.

## Layout

- `crates/`: the Rust workspace
  - `android`: what is true of Android whatever ports it: EROFS and ext4 images (raw or sparse) read in place, scanned into partition trees and built back, alone or straight into a `super` image; partition paths, mount points and Treble sides; Android file metadata and its defaults; `.prop` files
  - `smali`: smali parser and method edits
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
- [ ] Patch boot images (`boot`, `vendor_boot`) in Evelyn instead of beforehand, and check the first-stage fstab in `vendor_boot` as well as `vendor/etc`
- [ ] Instruction-level smali patches (replace a call or constant in every class): make the smali tree lossless, print it back, and edit it as a tree with visitors instead of splicing text, with the method-level edits rebuilt on top

## Authors

- [@Cyber-Phantom](https://github.com/Cyber-Phantom)
