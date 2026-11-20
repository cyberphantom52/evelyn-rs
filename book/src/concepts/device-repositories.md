# Device repositories

A **device repository** ports one ROM to one device.

## Layout

```text
evelyn.toml               the device: product ID, the stock build it is
                          pinned to, how partitions are built, its super
                          layout
ports/<name>.toml         one ROM version: the builds it is for, what it takes
                          from the ROM, and the patches it applies
patches/<name>/patch.ron  one patch, named for what it changes, with its
                          assets beside it
kits/*.ron                the repository's own kits: operations its patches
                          share
firmware/stock/           the device's stock firmware
firmware/rom/             the ROM being ported
out/<port>/               what a build writes
.evelyn/                  each build's working space
```

Evelyn finds the repository by looking for `evelyn.toml` in the current
directory, then in each parent directory, so `evelyn build` works from anywhere
inside the repository.

## What goes in git

`evelyn init` writes a `.gitignore` that leaves out `/.evelyn/`, `/firmware/`
and `/out/`. Firmware is large and usually cannot be redistributed. The
manifest, ports, patches and kits are what you commit.

None of these files names a particular firmware build. You supply the
firmware when you build. To port a new OTA of the same ROM, replace what
`firmware/rom` holds, or pass `--rom` to the build.

## Firmware

`firmware/rom` and `firmware/stock` each hold one of these:

- **Images:** a directory of `<partition>.img` files. They can be EROFS or ext4,
  raw or sparse.
- **An OTA package:** a full OTA zip holding `payload.bin`, or a bare
  `payload.bin`.

A directory that holds both a package and images is refused.

When the firmware is a package, a build extracts only the partitions it reads.
It reads them into scratch space that is removed afterwards, and checks each
one against the hash in the package's manifest. A build reads these partitions:

1. the ROM's system side, whose properties pick the port
2. what the port takes from the ROM, and what its patches read
3. the device's own partitions from the stock firmware

To replace a package with its images for good, run
[`evelyn extract`](../reference/cli.md#evelyn-extract). Builds then read the
images in place and no longer extract the package.

## The Treble split

Evelyn composes the build along Android's Treble boundary:

| Side   | Partitions                                         | Comes from      |
|--------|----------------------------------------------------|-----------------|
| Vendor | `vendor`, `odm`, `vendor_dlkm`, `odm_dlkm`, `system_dlkm` | stock firmware |
| System | `system`, `system_ext`, `product`                  | ROM             |
| Other  | e.g. Oplus's `my_*` partitions                     | ROM, if the port lists them |

`system_dlkm` is on the vendor side because it holds the GKI modules built
with the device's kernel: a ROM's are built for another kernel.

Every vendor-side partition is checked against the build fingerprint that
`evelyn.toml` pins. A mismatch fails the build.
