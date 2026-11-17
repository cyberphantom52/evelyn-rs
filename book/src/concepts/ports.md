# Ports

A **port** is one version of the ROM on the device. It is a file under
`ports/`, such as `ports/a15.toml`, and it says three things:

- which ROM builds the port is for
- which partitions it takes from the ROM, beyond the standard system side
- which patches it applies, and in what order

## Picking a port

You never name the port on the command line. A build reads the properties the
ROM sets, then takes the port whose `[rom]` table matches them. Each entry in
`[rom]` is a property key and a glob over its value:

```toml
[rom]
"ro.build.version.release" = "15"
"ro.build.display.id" = "*CPH2581*"
```

A port matches when every one of its entries matches. A port with no `[rom]`
table matches every build.

A new OTA of the same version is built by replacing the firmware. A build of the
next Android version picks that version's port, if
one matches.

## Patches belong to ports

A patch applies only where a port lists it:

- Two ROM versions share a patch by both listing it.
- A new port starts with no patches. You carry each one over and check it
  against the new ROM, rather than inheriting patches that may no longer fit.

```toml
patches = [
    "product-ids",
    "disable-ota-updater",
    "camera-features",
]
```

Patches apply in the order listed. A later patch sees what earlier ones
changed.

## Extra ROM partitions

The standard system side (`system`, `system_ext`, `product`, `system_dlkm`) is
always taken from the ROM. A ROM may have other partitions, such as Oplus's
`my_product` or `my_stock`. The port must list those in `partitions`, by
name or by glob, or they are left out:

```toml
partitions = ["my_*"]
```

A dry run reports which ROM partitions were left out. A pattern that matches a
vendor-side partition is an error, because those partitions come from the stock
firmware.

How each partition is built, its filesystem and mount point, does not depend
on the port. It is set in [`evelyn.toml`](../reference/evelyn-toml.md#partition-settings).
