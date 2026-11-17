# How a build works

`evelyn build` runs one pipeline:

1. **Open the firmware.** Evelyn opens `firmware/rom` and `firmware/stock`, or
   whatever `--rom` and `--stock` give: images, or an OTA package, local or
   remote.
2. **Pick the port.** It reads the ROM's system-side properties and takes the
   port whose `[rom]` globs match them.
3. **Resolve the partitions.** The vendor side comes from the stock firmware
   and is checked against `fingerprint` in `evelyn.toml`. The system side, and
   anything the port lists, comes from the ROM.
4. **Extract and scan.** Evelyn extracts each partition the build reads, if the
   firmware is a package. It then scans the partition into a tree whose files
   point back into the source image.
5. **Apply the recipes.** These run in order:
   1. the port's patches
   2. Evelyn's built-in recipes
   3. the debug recipe, with `--debug`
6. **Check the fstab.** Evelyn reads the composed `vendor/etc/fstab.*`. The build
   must supply every `logical` entry that is not `nofail`, in a filesystem that
   entry accepts.
7. **Build the images.** Evelyn writes each partition back out as EROFS or ext4.
   Untouched files stream straight from the source image. Only edited files
   come from disk.
8. **Publish.** Evelyn writes partition images, a `super` image, or a ZIP to
   `out/<port>/`.

## Nothing is unpacked

Each source image stays open for the whole build. A file no recipe touches is
never written to disk. It is copied from the old image into the new one as the
new image is built. Edits write their results into the work directory:

```text
.evelyn/
├── firmware/{rom,stock}/   partitions extracted from an OTA package
├── targets/<partition>/    the files that edits wrote
└── images/<partition>.img  built filesystem images
```

## Partition settings

Each built partition keeps its source image's filesystem and is mounted at
`/<name>`, unless settings say otherwise. `system` is mounted at `/` or
`/system`, depending on how its image is laid out. `[filesystems]` and
`[mount_points]` in [`evelyn.toml`](../reference/evelyn-toml.md#partition-settings)
override them, for partitions from either firmware.

## Outputs

| `--output`   | Writes                                                    | Default when                    |
|--------------|-----------------------------------------------------------|---------------------------------|
| `super`      | `super.img`, built to `[super]`                           | `evelyn.toml` has a super layout |
| `partitions` | one `<partition>.img` per partition                       | it has none                     |
| `zip`        | a recovery-flashable ZIP, named `<sha256>.zip`            | never; ask for it               |

The ZIP holds:

- a zstd-compressed `super` image
- the installer, which accepts only a device whose `ro.product.device` is
  `product_id`
- the device firmware outside `super`: the stock OTA's other partitions, or the
  images in `firmware/stock/firmware/`

> **Note:** ZIP output has not been flash-tested yet.

## Dry runs

`--dry-run` runs every step through the fstab check, then stops before it
builds images. It prints what the build composed:

- the port
- each partition and its source
- the patches in order
- the ROM partitions left out
- the steps skipped
