# evelyn.toml

`evelyn.toml` sits at the root of a device repository and describes the
device. It is the same for every ROM version the repository ports. Evelyn
rejects unknown fields.

<span class="filename">Filename: evelyn.toml</span>

```toml
product_id = "pong"
fingerprint = "vendor/pong/pong:14/BUILD.ID/1234567:user/release-keys"

[filesystems]
system = "ext4"
"my_*" = "erofs"

[mount_points]
my_product = "/my_product"

[super]
size = 9663676416
metadata_size = 65536
slots = "virtual_ab"
group = "main"
```

## Top level

| Key           | Type   | Required | Meaning |
|---------------|--------|----------|---------|
| `product_id`  | string | yes, non-empty | The `ro.product.device` value the ZIP installer accepts. |
| `fingerprint` | string | yes, non-empty | The stock build the device's partitions must come from. Each vendor-side partition's `ro.<partition>.build.fingerprint` must equal it. |

## `[filesystems]`

The filesystem to build a partition with, where the source image's is wrong.
Keys are partition names or globs, and values are `erofs` or `ext4`. Without an
entry, a partition keeps its source image's filesystem.

## `[mount_points]`

Where a partition is mounted, where the default is wrong. Keys are partition
names or globs, and values must be absolute paths. Without an entry, a
partition is mounted at `/<name>`, and `system` at `/` or `/system`, depending
on how its image is laid out.

### Partition settings

`[filesystems]` and `[mount_points]` apply to every partition the build
composes, from either firmware. When several keys match a partition, the most
specific one wins:

1. an exact name beats any glob
2. among globs, the one with more literal characters wins

Two equally specific globs with different values are an error.

## `[super]`

This table is optional. Without it, a build writes partition images by default,
and cannot write `super` or ZIP output.

| Key             | Type    | Meaning |
|-----------------|---------|---------|
| `size`          | integer | The `super` partition's size in bytes. |
| `metadata_size` | integer | The size of each copy of the LP metadata, in bytes. This is usually `65536`. |
| `slots`         | string  | `single`, `ab` or `virtual_ab`. |
| `group`         | string  | The one dynamic partition group, as the vendor names it. Examples are `main` and `qti_dynamic_partitions`. |

On `ab`, each slot holds a full copy of the group, so the group may use half of
`size`. On `single` and `virtual_ab`, it may use all of it.
