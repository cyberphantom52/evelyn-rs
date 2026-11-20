# Your first port

A port is built up in small steps: create a device repository, describe the
device, add a port with no patches, check it with a dry run, and then add
patches one at a time. Each dry run shows exactly what the build composed.

The examples port OxygenOS to a device named `pong`.

## Creating a device repository

```console
$ evelyn init pong-oxygenos
$ cd pong-oxygenos
```

`evelyn init` creates the repository's directories and its manifest, then runs
[`evelyn check`](./installation.md#evelyn-check). It prints both results:

```json
{
  "repository": {
    "root": "pong-oxygenos",
    "manifest": "pong-oxygenos/evelyn.toml"
  },
  "checked": [ … ]
}
```

The repository starts out like this:

```text
pong-oxygenos/
├── evelyn.toml
├── ports/
├── patches/
├── firmware/
│   ├── stock/
│   └── rom/
└── .gitignore
```

## Describing the device

`evelyn.toml` describes the device. Two fields have no default and must be
filled in: `product_id` and `fingerprint`.

<span class="filename">Filename: evelyn.toml</span>

```toml
product_id = "pong"
fingerprint = "vendor/pong/pong:14/BUILD.ID/1234567:user/release-keys"

[super]
size = 9663676416
metadata_size = 65536
slots = "virtual_ab"
group = "main"
```

- **`product_id`** is the `ro.product.device` value the ZIP installer
  accepts.
- **`fingerprint`** pins the stock build. Every vendor-side partition's
  `ro.<partition>.build.fingerprint` must equal it. On the device, it is
  `getprop ro.vendor.build.fingerprint`.
- **`[super]`** describes the `super` partition. It is needed for
  `super` and ZIP output. The values come from the stock `super` image's
  metadata, for example as `lpdump` prints them.

The fingerprint and `super` values above are placeholders.
[evelyn.toml](../reference/evelyn-toml.md) lists every field.

## Adding the firmware

The stock firmware goes in `firmware/stock` and the ROM in `firmware/rom`. Each
is a full OTA package, or a directory of `<partition>.img` images:

```text
firmware/
├── stock/
│   └── pong-stock-ota.zip
└── rom/
    └── oxygenos-ota.zip
```

A build extracts only the partitions it reads from a package, and removes them
afterwards. `evelyn extract` replaces a package by its images for good.

## Adding a port

A port is one ROM version, in a file under `ports/`. It says which ROM builds it is for,
which ROM partitions it takes beyond the standard system side, and which
patches it applies. This one applies none yet:

<span class="filename">Filename: ports/a15.toml</span>

```toml
partitions = ["my_*"]
patches = []

[rom]
"ro.build.version.release" = "15"
```

- **`[rom]`** matches ROM builds: each key is a property the ROM sets, and each
  value a glob over it. A build takes the port that matches.
- **`partitions`** takes every `my_*` partition from the ROM, as well as
  `system`, `system_ext` and `product`.
- **`patches`** lists the patches to apply. Both lists must come before the
  `[rom]` header, because TOML puts every key after a header into that table.

## Checking the port with a dry run

A dry run applies every recipe to the firmware, then stops before it builds
any image:

```console
$ evelyn build --dry-run
```

```json
{
  "port": "a15",
  "partitions": [
    { "name": "system", "from": "Rom", "filesystem": "erofs", "mount_point": "/" },
    { "name": "system_ext", "from": "Rom", "filesystem": "erofs", "mount_point": "/system_ext" },
    { "name": "product", "from": "Rom", "filesystem": "erofs", "mount_point": "/product" },
    { "name": "my_product", "from": "Rom", "filesystem": "erofs", "mount_point": "/my_product" },
    { "name": "vendor", "from": "Stock", "filesystem": "erofs", "mount_point": "/vendor" },
    { "name": "odm", "from": "Stock", "filesystem": "erofs", "mount_point": "/odm" }
  ],
  "recipes": ["vndk.ron"],
  "ignored": [],
  "skipped": [
    {
      "recipe": "vndk.ron",
      "step": "vndk-apex",
      "reason": "vndk is not set: Property(Partition(\"vendor\"),\"ro.vndk.version\")"
    }
  ],
  "outputs": []
}
```

- **`port`** is the port the ROM matched.
- **`partitions`** lists each partition the build composes, the firmware it
  comes from, its filesystem and its mount point, in `super` order.
- **`recipes`** lists every recipe applied, in order. With no patches, only
  Evelyn's built-in `vndk.ron` applies.
- **`ignored`** lists ROM partitions the port does not take.
- **`skipped`** lists what did not apply, and why. This vendor sets no
  `ro.vndk.version`, as on Android 15 and later, so the VNDK step skips the
  rest of its operations.
- **`outputs`** is empty, because a dry run writes nothing.

## Writing a patch

A patch is a directory under `patches/` holding a `patch.ron`. This one makes
the ROM's system side report the device's own product name:

<span class="filename">Filename: patches/product-device/patch.ron</span>

```ron
[
    Step("product-device", [
        Let("device", Property(Partition("vendor"), "ro.product.vendor.device")),
        Props(All, [Set("ro.product.system.device", "{device}")]),
    ]),
]
```

- **`Step("product-device", [ … ])`** is a named list of operations, applied
  in order.
- **`Let`** reads `ro.product.vendor.device` from the stock `vendor`
  partition's property files and binds it to `device`.
- **`Props(All, [Set(…)])`** sets `ro.product.system.device` to that value in
  every property file that defines it.

A patch applies only where a port lists it:

<span class="filename">Filename: ports/a15.toml</span>

```toml
patches = ["product-device"]
```

The next dry run applies it before the built-in recipes:

```console
$ evelyn build --dry-run
```

```json
{
  "port": "a15",
  …
  "recipes": ["patches/product-device/patch.ron", "vndk.ron"],
  …
}
```

## Building

```console
$ evelyn build
```

```json
{
  "port": "a15",
  …
  "outputs": ["out/a15/super.img"]
}
```

With a `[super]` layout, the default output is a `super` image. A
recovery-flashable ZIP is one flag away:

```console
$ evelyn build --output zip
```

## Summary

- A device repository holds `evelyn.toml`, the ports and the patches. The
  firmware is supplied at build time.
- The ROM's properties pick the port.
- A patch is a recipe of named steps, and applies only where a port lists it.
- `evelyn build --dry-run` shows what a build composes without building it.
