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
ports/<name>/port.toml one ROM version: the ROM builds it is for, what it takes from the ROM, and the patches it applies
patches/<name>/patch.ron  one patch, named for what it changes, with its assets beside it
kits/*.ron             the repository's own kits: operations its patches share
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

After the port's patches, every build applies what Evelyn does for any port:
the VNDK APEX a vendor older than Android 15 needs, when the build lacks it.

A port takes the device's own partitions (`vendor`, `odm`, `vendor_dlkm`,
`odm_dlkm`) from the stock firmware, checked against the build fingerprint
`evelyn.toml` pins, and `system`, `system_ext`, `product` and `system_dlkm` from the
ROM. Other ROM partitions are taken only when the port lists them.

## Recipes

A patch's `patch.ron` is a recipe: a list of steps, applied in the order they
are written, each a name and a list of operations, applied in order.

```ron
[
    Step("vndk-apex", [
        Let("vndk", Property(Partition("vendor"), "ro.vndk.version")),
        When(Not(Exists(Build("/system_ext/apex/com.android.vndk.v{vndk}.apex"))), [
            Remove("/system_ext/apex/com.android.vndk*.apex"),
            Install("apex/com.android.vndk.v{vndk}.apex", "/system_ext/apex/com.android.vndk.v{vndk}.apex"),
        ]),
    ]),
]
```

`Let(name, value)` binds a name to a string read from the build so far, which the
strings of the operations after it name as `{name}`; one that is not set skips
them and is reported. Paths are device paths (`/vendor/etc/fstab.qcom`), globs
allowed, and always start with `/`; asset paths are relative to the patch's
directory. Write a regular expression or a string full of quotes raw: `r"a\.b"`,
`r#"<a name="b"/>"#`. Beyond its syntax, nothing is checked before a recipe is
applied: a wrong path or value fails the build at its step.

The operations change the build, or inside a `Package`, the package:

- `Copy(from:, to:, metadata: Source | Default, overwrite:, create_parent:)`
  copies from a firmware, `Rom("/…")` or `Stock("/…")`, from the build,
  `Build("/…")`, or an asset, `Asset("…")`. `Import(Rom("/…"))` is a copy to the
  same path with the firmware's metadata; `Install(asset, to)` a copy of an
  asset with Android's default metadata.
- `Remove("/…")` removes what a path matches. Matching nothing is reported, not
  an error.
- `SetMetadata(pattern:, uid:, gid:, mode:, selinux:)` sets what Android's
  defaults do not give.
- `Props(scope, [Set(key, value), Remove(key)])` edits properties: `All` of the
  build's property files, as init loads them, a `Partition("…")`'s, or one
  `Build("/…")` file. A key is set in every file of the scope that defines it,
  else added where init loads it last, or to the partition's `build.prop`; a
  key removed that none defines is reported.
- `Text(path, [Append(text), Diff(asset)])`, `Bytes(path, {offset: "hex"})` and
  `Xml(path, [SetText, SetAttribute, Remove, PutChild])` edit a file.
- `Policy(path, [Add(asset), Import(types:, from:)])` patches one of the vendor's
  CIL files, checked to resolve with the rest of the device's policy. `Add` adds
  a TE policy; policies added one after another are added together, so one can
  use what another declares. `Import` adds what the compiled policy `from` (the
  ROM's `/odm/etc/selinux/precompiled_sepolicy` by default) has for `types`,
  names or globs: their declarations, less what the device's policy already
  declares, and the rules that name them. A rule needing a name no policy
  defines is left out until a later import defines it; what is still left out
  at the end is reported with its step. `RUST_LOG=evelyn::patch::operations=debug`
  prints each import.
- `Package(path, Signed | Unsigned, [ … ])` applies operations to an APK or JAR,
  and signs the rebuilt package with Evelyn's key or not, as an APK Android
  verifies needs. Inside it, paths are from its root, as the package has them
  (`/assets/…`, `/lib/…`), except that its resources and manifest are text
  (`/res/values/strings.xml`, `/AndroidManifest.xml`). Only what they change is
  put back: a new file under `/res/` adds its resource, and a removed one is
  removed. `Resources([Set(type:, name:, config:, value:, create:)])` sets a values
  resource (`string`, `bool`, `integer`, `color`, `dimen`, `fraction`,
  `string-array`, `integer-array`) in a configuration (`""` for `values/`, or a
  qualifier like `"en-rIN"`), whose file must be there: one that is not is an
  error, or with `create: true`, added. A value is what goes between the tags, an
  array's a list of its items. Its code is edited as smali, in place:
  `StubMethod`, `ReplaceMethod` and `InsertMethod` change one method of a class,
  and `ReplaceOperand(from:, to:)` replaces what instructions name (a method, a
  field, a type, a string, a number) in every class, or in `class:`, or in its
  `method:`.
- `Assert(condition, message)` and `AssertHash(path, sha256)` fail the build
  when the firmware is not what the patch expects.
- `ForEach(over, name, [ … ])` applies operations once for each string of a
  list, or each file of a selection, with `{name}` bound to it. What its
  operations report for every one is reported once. `ForEach(Partitions(Rom),
  "p", [Remove("{p}/app/Foo")])` removes from every partition the ROM supplies.
- `When(condition, [ … ])` applies operations if the condition holds.

Expressions select files (`Build`, `Rom`, `Stock`, `Asset`, `Package`), read them
(`Read`, `Property(scope, key)`, `Lines`), list the directories of the
partitions a firmware supplies (`Partitions(Rom | Stock)`), test values
(`Exists`, which also takes a `Partition(…)`, `Not`, `All`, `Matches(text,
"regex")`, `Contains(text, "…")`) and combine them (`Union`, `Filter(over:,
name:, keep:)`, `Join`).

### Kits

A kit defines operations out of the operations above. Evelyn ships kits for ROM
families, in `assets/kits/`, and a device repository can have its own, in
`kits/*.ron`, for what several of its patches do alike. Every recipe can call
any kit's operation as it calls a built-in one: the one parameter it must give
bare, or any by name, those with a default included. An asset a kit's operation
names is the calling patch's.

```ron
Kit(
    operations: {
        "Labels": (
            parameters: ["service", "exec"],
            operations: [
                Text("/vendor/etc/selinux/vendor_service_contexts", [Append("{service}")]),
                Text("/vendor/etc/selinux/vendor_file_contexts", [Append("{exec}")]),
            ],
        ),
    },
)
```

A kit also names, in `property_files`, the property files its ROM family's init
loads after Android's, in the order it loads them. A property read of `All`, and
a new key's file, follow that order; a property file outside it that gives a
read key another value is reported. `oplus.ron` names each `my_*` partition's
`build.prop`.

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

What else a build needs Evelyn keeps in its data directory
(`~/Library/Application Support/evelyn` on macOS, `~/.local/share/evelyn` on
Linux): VNDK APEXes, downloaded the first time one is
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
build. `--debug` makes a debug build: after every other recipe, one built into
Evelyn writes logcat and the kernel log to `/data/local/tmp/logs` until boot
completes, and turns adb on from boot without authorisation. `--dry-run` applies every patch to the firmware
but writes no images, and prints what the build composed: the port, each
partition and the firmware it comes from, the patches in order, the ROM
partitions left out and the steps skipped.

## Layout

- `crates/`: the Rust workspace
  - `android`: what is true of Android whatever ports it: EROFS and ext4 images (raw or sparse) read in place, scanned into partition trees and built back, alone or straight into a `super` image; partition paths, mount points and Treble sides; Android file metadata and its defaults; `.prop` files
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
- [ ] Patch boot images (`boot`, `vendor_boot`) in Evelyn instead of beforehand, and check the first-stage fstab in `vendor_boot` as well as `vendor/etc`
- [ ] Take a HAL from a donor firmware: list the files it needs that the device lacks (its libraries' closure, checked against the device's vendor), and derive its SELinux policy from the donor's compiled policy, keeping the rules whose types the device's policy defines and reporting the rest
- [ ] Inventory a port's HALs: those the ROM's system side uses, against those the device's vendor declares (VINTF manifests and vendor APEXes)

## Authors

- [@Cyber-Phantom](https://github.com/Cyber-Phantom)
