# Apply a diff to a config file

<span class="filename">Filename: patches/fstab-ext4/fstab.diff</span>

```diff
--- a/vendor/etc/fstab.qcom
+++ b/vendor/etc/fstab.qcom
@@ -1,3 +1,4 @@
 system    /system    erofs  ro  wait,slotselect,avb=vbmeta_system,logical,first_stage_mount
+system    /system    ext4   ro  wait,slotselect,avb=vbmeta_system,logical,first_stage_mount
 vendor    /vendor    erofs  ro  wait,slotselect,avb,logical,first_stage_mount
 product   /product   erofs  ro  wait,slotselect,avb,logical,first_stage_mount
```

<span class="filename">Filename: patches/fstab-ext4/patch.ron</span>

```ron
[
    Step("fstab", [
        Text("/vendor/etc/fstab.qcom", [Diff("fstab.diff")]),
    ]),
]
```

- **`Text(path, [ … ])`** opens a file as text and applies its edits in order.
- **`Diff(asset)`** applies a unified diff from the patch's directory to the
  file and to its hardlinks. A diff that does not apply fails the build.

To add a line without a diff, append it:

```ron
Text("/vendor/etc/init/hw/init.target.rc", [Append("import /vendor/etc/init/extra.rc")])
```

`Append` also takes text read from an asset:
`Append(Read(Asset("extra.rc")))`.
