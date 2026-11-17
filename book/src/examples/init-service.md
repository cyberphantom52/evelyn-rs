# Install an init service

A daemon, its init script and its SELinux label:

```text
patches/mydaemon/
├── patch.ron
├── mydaemon
└── mydaemon.rc
```

<span class="filename">Filename: patches/mydaemon/mydaemon.rc</span>

```text
service mydaemon /vendor/bin/mydaemon
    class main
    user system
    group system
```

<span class="filename">Filename: patches/mydaemon/patch.ron</span>

```ron
[
    Step("mydaemon", [
        Install("mydaemon", "/vendor/bin/mydaemon"),
        SetMetadata(pattern: "/vendor/bin/mydaemon", mode: 0o755, selinux: "u:object_r:mydaemon_exec:s0"),
        Install("mydaemon.rc", "/vendor/etc/init/mydaemon.rc"),
    ]),
]
```

- **`Install`** copies an asset from the patch's directory to a device path,
  with Android's default owner, mode and SELinux label for that path. It
  replaces a file already there, and makes missing parent directories.
- **`SetMetadata`** sets what the defaults do not give: here, an executable
  mode and the daemon's own label.

The `mydaemon_exec` type must exist in the device's policy.
[Add SELinux policy for a daemon](./policy-add.md) declares it.
