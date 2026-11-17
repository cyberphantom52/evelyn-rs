# Troubleshooting

Each Evelyn error says what Evelyn was doing, then lists its causes, one
`caused by:` line each.

## Repository and port errors

- **`… is not in a device repository: no evelyn.toml here or above`**\
  Run `evelyn build` from inside the repository, or run `evelyn init` first.

- **`evelyn.toml sets no product_id`**\
  Fill in `product_id` with the device's `ro.product.device`.

- **`evelyn.toml sets no fingerprint`**\
  Fill in the stock build fingerprint. See [evelyn.toml](../reference/evelyn-toml.md#top-level).

- **`unknown field …`**\
  Older repositories had a `[device]` table in `evelyn.toml`, and a
  `ports/<name>/port.toml` with a `[partitions]` table. See
  [evelyn.toml](../reference/evelyn-toml.md) and
  [port files](../reference/port-toml.md) for the current layout.

- **`port partition my_* matches vendor, which comes from the stock firmware`**\
  The glob in the port's `partitions` is too broad. Narrow it so it no longer
  matches a vendor-side partition.

- **`mount point for … must be absolute`**\
  Each value in `[mount_points]` must start with `/`.

- **`partition … matches A and B in filesystems equally, with different values`**\
  Two globs are equally specific. Make one more specific, or name the
  partition exactly.

- **`the ROM matches no port`**, **`the ROM matches more than one port`**\
  Check the `[rom]` table of each file in `ports/` against the ROM's
  properties.

- **`patch … has no patches/…/patch.ron`**, **`patch … is listed twice`**, **`patch … is not a directory name`**\
  Check the port's `patches` list. Each name is a directory directly under
  `patches/`.

## Recipe errors

- **`invalid recipe …`**\
  The RON does not parse, or it uses a field that does not exist. The message
  gives the line and column.

- **`{name} is not bound`**\
  A string uses `{name}`, but nothing binds that name before it. A `Let` binds only for the operations
  after it in the same list, so a `Let` inside a `When` is not seen outside it.

- **`… is bound to "…", which is not [A-Za-z0-9._-]`**\
  A `Let` value may contain only letters, digits, `.`, `_` and `-`. Bind a
  simple value, such as a version property. Do not bind file contents or
  free text.

- **`Remove takes a path from the root, /…, not …`**\
  `Remove` no longer tries a relative path in each partition. Write the full
  device path. To remove from every ROM partition, use
  `ForEach(Partitions(Rom), "p", [Remove("{p}/…")])`.

- **`… is already an operation`**\
  A kit names an operation the same as a built-in one, or the same as another
  kit's operation. Rename it.

## Steps that were skipped

A skip is not an error. When a `Let` value is unset, the operations after it
are skipped. Each skip is logged as a warning, and listed in the
build's JSON output under `skipped`. Run `evelyn build --dry-run` to see them
without building images.

## Still stuck?

[Open an issue](https://github.com/cyberphantom52/evelyn-rs/issues). Include
the full error output, and the port and patch involved.
