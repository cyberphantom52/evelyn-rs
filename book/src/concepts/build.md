# How a build works

`evelyn build` runs one pipeline:

1. **Open the firmware.** Evelyn opens `firmware/rom` and `firmware/stock`, or
   whatever `--rom` and `--stock` give: images, or an OTA package, local or
   remote.
2. **Pick the port.** It reads the ROM's system-side properties and takes the
   port whose `[rom]` globs match them.
3. **Resolve the partitions.** The vendor side comes from the stock firmware
   and is checked against `fingerprint` in `evelyn.toml`. The system side, and
   anything the port lists, comes from the ROM. The ramdisks of the stock
   [boot images](#boot-images) come from the stock firmware.
4. **Extract and scan.** Evelyn extracts each partition the build reads, if the
   firmware is a package. It then scans the partition into a tree whose files
   point back into the source image.
5. **Apply the recipes.** These run in order:
   1. the port's patches
   2. Evelyn's built-in recipes
   3. the debug recipe, with `--debug`
   4. the permissive SELinux recipe, with `--selinux-permissive`
6. **Check the fstab.** Evelyn reads the composed `vendor/etc/fstab.*`, and the
   first-stage `fstab.*` in `vendor_boot`. The build must supply every
   `logical` entry that is not `nofail`, in a filesystem that entry accepts.
7. **Build the images.** Evelyn writes each partition back out as EROFS or ext4.
   Untouched files stream straight from the source image. Only edited files
   come from disk. A boot image is built again only if a recipe changed it.
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

## Boot images

A recipe edits the stock `boot`, `init_boot` and `vendor_boot` images in a
[`BootImage`](../reference/operations.md#bootimage), whose paths are paths in
the image's ramdisk:

```ron
BootImage("vendor_boot", [
    Text("/first_stage_ramdisk/fstab.qcom", [Append(Read(Asset("fstab.my_partitions")))]),
])
```

On the device, these ramdisks are unpacked together into the rootfs that
first-stage init runs in, before `system` is mounted at `/`. They are not
mounted anywhere, so a path outside a `BootImage` never reaches them. Evelyn
finds the images beside the stock firmware's filesystems, in its `firmware/`
directory, or in its OTA package. A dry run lists them as `boot_images`.

A `vendor_boot` image's bootconfig, which init gives as `ro.boot.*`
properties, is edited with [`Bootconfig`](../reference/operations.md#bootconfig)
in its `BootImage`.

A boot image whose ramdisk or bootconfig a recipe changes is built again from
the stock one, with the new ramdisk compressed as the old one was. Its AVB footer, and a GKI
boot signature, are re-signed with the AOSP test key of the original key's
size. A new file in a ramdisk gets owner and
group `root`, and mode `0644`, or `0755` for a directory, unless the recipe sets
other metadata. Ramdisk entries have no SELinux labels.

A boot image is left as it is, with a warning, if Evelyn cannot read its
ramdisk. This happens when the image has no ramdisk, has several (some
`vendor_boot` images do), or holds device nodes.

## Verified boot

Android checks every partition it boots against the stock firmware's `vbmeta`
images, which hold each partition's hash or dm-verity hashtree, signed with the
device maker's key. A built partition no longer matches them, so Evelyn rebuilds
them, as a custom ROM's build does:

- Each built partition gets a dm-verity hashtree and an AVB footer after its
  filesystem, as `avbtool add_hashtree_footer` writes them.
- Each stock `vbmeta` image (`vbmeta`, `vbmeta_system`, ...) is rebuilt with
  the built partitions' hashtrees and the rebuilt boot images' hashes in place
  of the stock ones. Its chains to images Evelyn signed take their new keys.
  Everything else is kept, including its flags.
- Each is signed with the AOSP test key of its original key's size.

The fstab's `avb` entries stay as they are, and dm-verity checks every
partition as the device reads it. An unlocked bootloader boots images signed
with any key, in its orange state; a locked one boots only the device maker's.

## Partition settings

Each built partition keeps its source image's filesystem and is mounted at
`/<name>`, unless settings say otherwise. `system` is mounted at `/` or
`/system`, depending on how its image is laid out. `[filesystems]` and
`[mount_points]` in [`evelyn.toml`](../reference/evelyn-toml.md#partition-settings)
override them, for partitions from either firmware.

## Outputs

| `--output`   | Writes                                                    | Default when                    |
|--------------|-----------------------------------------------------------|---------------------------------|
| `super`      | `super.img`, built to `[super]`, the changed boot images and the `vbmeta` images | `evelyn.toml` has a super layout |
| `partitions` | one `<partition>.img` per partition, changed boot image and `vbmeta` image | it has none                     |
| `zip`        | a recovery-flashable ZIP, named `<sha256>.zip`            | never; ask for it               |

The ZIP holds:

- a zstd-compressed `super` image
- the installer, which accepts only a device whose `ro.product.device` is
  `product_id`
- the device firmware outside `super`: the stock OTA's other partitions, or the
  images in `firmware/stock/firmware/`, with the changed boot images and the
  rebuilt `vbmeta` images in place of their stock ones

> **Note:** ZIP output has not been flash-tested yet.

## Dry runs

`--dry-run` runs every step through the fstab check, then stops before it
builds images. It prints what the build composed:

- the port
- each partition and its source
- the patches in order
- the ROM partitions left out
- the steps skipped
