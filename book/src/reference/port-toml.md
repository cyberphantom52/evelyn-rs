# Port files

Each port is a file `ports/<name>.toml`. The port's name is the file's name
without `.toml`, and builds write to `out/<name>/`. Evelyn rejects unknown
fields.

<span class="filename">Filename: ports/a15.toml</span>

```toml
partitions = ["my_*"]

patches = [
    "product-device",
    "camera-features",
]

[rom]
"ro.build.version.release" = "15"
```

> **Note:** Put `partitions` and `patches` before the `[rom]` header. TOML puts
> every key after a header into that table.

## `partitions`

The ROM partitions the port takes beyond the standard system side, by name or
glob. The standard system side, `system`, `system_ext`, `product` and
`system_dlkm`, is always taken. A ROM partition no entry matches is left out,
and listed as `ignored` in the build's output.

A name or glob that matches a vendor-side partition, `vendor`, `odm`,
`vendor_dlkm` or `odm_dlkm`, is an error. Those always come from the stock
firmware.

How a partition is built, its filesystem and mount point, is set in
[`evelyn.toml`](./evelyn-toml.md#partition-settings).

## `patches`

A list of patch names, in the order they apply. Each name is a directory
directly under `patches/` that holds a `patch.ron`.

The build fails on any of these:

- a name that contains `/` or `\`, or is empty, `.` or `..`
- a name listed twice
- a patch with no `patches/<name>/patch.ron`

## `[rom]`

Property keys mapped to globs. The port is for every ROM build that sets each
key to a value its glob matches. With no `[rom]` table, the port is for every
build.

```toml
[rom]
"ro.build.version.release" = "15"
"ro.build.version.incremental" = "V816.0.*"
```

A build takes the one port the ROM matches. A ROM that matches no port, or more
than one, fails the build.
