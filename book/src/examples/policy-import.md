# Import a HAL's policy from the ROM

A HAL taken from the ROM needs its policy too. The ROM's
`/odm/etc/selinux/precompiled_sepolicy` has its vendor policy compiled in.
`Import` copies what that policy has for some types:

<span class="filename">Filename: patches/camera-hal/patch.ron</span>

```ron
[
    Step("camera-hal-policy", [
        Policy("/vendor/etc/selinux/vendor_sepolicy.cil", [
            Import(types: ["hal_camera_example", "hal_camera_example_*"]),
        ]),
    ]),
]
```

- **`Import(types: [ … ])`** adds, for each type or attribute the names or
  globs match, its declaration and the rules that name it. What the device's
  policy already declares is not declared again.
- **`from:`** defaults to `Rom("/odm/etc/selinux/precompiled_sepolicy")`. Any
  compiled policy can be given:

```ron
Policy("/vendor/etc/selinux/vendor_sepolicy.cil", [
    Import(types: ["hal_camera_example"], from: Rom("/vendor/etc/selinux/precompiled_sepolicy")),
])
```

A rule that names a type nothing declares yet is held back, in case a later
`Import` declares it. What is still held back at the end of the build is left
out and reported. Each import is logged at the debug level:

```console
$ RUST_LOG=evelyn::patch::operations=debug evelyn build --dry-run
```
