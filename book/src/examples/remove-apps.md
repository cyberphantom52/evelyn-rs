# Remove apps from the ROM

<span class="filename">Filename: patches/debloat/patch.ron</span>

```ron
[
    Step("debloat", [
        ForEach(Partitions(Rom), "p", [
            Remove("{p}/app/Facebook*"),
            Remove("{p}/priv-app/OplusOTA"),
            Remove("{p}/del-app/*"),
        ]),
    ]),
]
```

- **`Partitions(Rom)`** lists the mount points of every partition the build
  took from the ROM: `/system`, `/system_ext`, `/product`, `/my_product`, and
  so on.
- **`ForEach`** binds each one to `p`, so each `Remove` is tried in every ROM
  partition.
- **`Remove`** takes a device path, globs allowed. A path that matches nothing
  is reported, not an error. Inside a `ForEach`, a path that matches nothing in
  *every* partition is reported once.

To remove a path in one known place, name it directly:

```ron
Remove("/product/app/YouTube")
```
