# Operations

Operations change the build, meaning the partitions being produced. Inside a
[`Package`](./packages.md), they change the package instead. Paths are device
paths that start with `/`, and globs are allowed where an operation matches
files.

Most operations take their arguments by position:
`Remove("/system/app/Browser")`. A few take them by name, for example `Copy`,
`SetMetadata` and the smali edits.

Several operations open a file as a **document** of one kind and apply that
kind's edits to it, in order:

| Operation | Opens the file as |
|-----------|-------------------|
| `Props`   | property files |
| `Text`    | text |
| `Bytes`   | bytes |
| `Xml`     | XML |
| `Policy`  | a CIL SELinux policy |

## Files

### `Copy`

```ron
Copy(from: Rom("/my_product/etc/camera/*.json"), metadata: Source, overwrite: true)
Copy(from: Asset("overlay"), to: "/product/overlay", metadata: Default, create_parent: true)
```

Copies what `from` names into the build, either at the same path or at `to`.

| Field           | Default   | Meaning |
|-----------------|-----------|---------|
| `from`          | —         | `Rom("/…")`, `Stock("/…")`, `Build("/…")` or `Asset("…")`. An asset can be a file or a directory, and needs `to`. |
| `to`            | same path | The destination device path. A glob keeps each match's path. |
| `metadata`      | —         | `Source` keeps the source's owner, mode and SELinux label. `Default` uses Android's defaults for the destination. |
| `overwrite`     | `false`   | Replace an entry that is already there. |
| `create_parent` | `false`   | Make a missing parent directory. |

What `from` names must exist. Every file the copy writes, and every directory
it makes, gets `metadata`. An asset has no metadata of its own, so `Source`
with an asset is an error.

### `Import`

```ron
Import(Rom("/my_stock/app/OplusCamera"))
```

Copies from a firmware to the same path in the build, with the firmware's
metadata. It is `Copy` with `metadata: Source`, `overwrite: true` and
`create_parent: true`.

### `Install`

```ron
Install("init/myservice.rc", "/vendor/etc/init/myservice.rc")
```

`Install(asset, to)` writes an asset, a file or a directory's files, at `to`,
with Android's default metadata. It is `Copy` with `metadata: Default`,
`overwrite: true` and `create_parent: true`.

### `Remove`

```ron
Remove("/system/app/Browser")
Remove("/product/app/*Updater*")
ForEach(Partitions(Rom), "p", [Remove("{p}/app/Facebook")])
```

`Remove(path)` removes what the path matches. The path must start with `/`. A
path that matches nothing is reported, not an error.

To remove the same thing from several partitions, use `ForEach` over
[`Partitions(Rom)`](./expressions.md#lists-and-selections) or
`Partitions(Stock)`.

### `SetMetadata`

```ron
SetMetadata(pattern: "/vendor/bin/mydaemon", uid: 0, gid: 2000, mode: 0o755, selinux: "u:object_r:mydaemon_exec:s0")
```

Sets metadata that Android's defaults do not give. It applies to every entry
`pattern` matches. Every field except `pattern` is optional: `uid`, `gid`,
`mode` and `selinux`.

## Properties

### `Props`

```ron
Props(All, [
    Set("ro.product.system.model", "Pong"),
    Remove("ro.oplus.ota.check"),
])
Props(Partition("vendor"), [Set("persist.sys.usb.config", "mtp,adb")])
Props(Build("/vendor/build.prop"), [Set("ro.secure", "0")])
```

`Props(scope, [edits])` edits properties. The scope says which property files
the edits apply to:

| Scope               | Files | A key no file defines is added to |
|---------------------|-------|-----------------------------------|
| `All`               | Every property file of the build, as init loads them. | the file init loads **last**, so the value takes effect |
| `Partition("name")` | The files of that partition that init loads. | the partition's own `build.prop` |
| `Build("/…")`       | That one file. | that file |

The edits:

- **`Set(key, value)`** sets the key in every file of the scope that defines
  it. If none defines it, the key is added to the file the table gives.
- **`Remove(key)`** removes every definition of the key in the scope. A key
  that no file defines is reported, not an error.

[Kits](./kits.md) can extend the list of files init loads. For example, the
Oplus kit adds the `my_*/build.prop` files.

## Editing file contents

### `Text`

```ron
Text("/vendor/etc/init/hw/init.qcom.rc", [Diff("init.qcom.rc.diff")])
Text("/vendor/etc/selinux/vendor_file_contexts", [
    Append(r"/vendor/bin/mydaemon u:object_r:mydaemon_exec:s0"),
])
```

`Text(path, [edits])` edits a file as text. The edits:

- **`Append(text)`** adds a string expression at the end of the file, on a
  line of its own. The text can be read from an asset:
  `Append(Read(Asset("extra.rc")))`.
- **`Diff(asset)`** applies a unified diff, which is an asset, to the file and
  to its hardlinks.

### `Bytes`

```ron
AssertHash("/vendor/lib64/libfoo.so", "<64 hex digits>"),
Bytes("/vendor/lib64/libfoo.so", { 487572: "a9008052" }),
```

`Bytes(path, {offset: "hex"})` overwrites bytes at offsets in a file. What a
byte offset means depends on the exact file, so put an `AssertHash` of the
file before it.

### `Xml`

```ron
Xml("/product/etc/sysconfig/foo.xml", [
    Remove(path: "//feature[@name='x']"),
])
```

`Xml(path, [edits])` applies [XML edits](./xml.md) to the file, in order.

## SELinux

### `Policy`

```ron
Policy("/vendor/etc/selinux/vendor_sepolicy.cil", [
    Add("sepolicy/mydaemon.te"),
    Import(types: ["hal_camera_oplus*"]),
])
```

`Policy(path, [edits])` patches one of the vendor's CIL policy files. The
result must resolve with the device's other policy files, as init loads them,
or the build fails. The edits:

- **`Add(asset)`** adds a TE policy written with Android's policy macros, such
  as `allow`, `domain_auto_trans` and `hal_client_domain`. Policies added one
  after another are added together, so one policy can use what another
  declares.
- **`Import(types:, from:)`** copies policy from a compiled policy. `types` is
  a list of type and attribute names, globs allowed. For each one, `Import`
  adds:
  - its declaration, unless the device's policy already declares it
  - the rules that name it

  `from` defaults to the ROM's `Rom("/odm/etc/selinux/precompiled_sepolicy")`.

A rule that needs a name no policy defines is held back, in case a later
`Import` defines it. Any rule still held back at the end of the build is left
out and reported, with the step that tried to import it. To see each import as
it happens, run:

```console
$ RUST_LOG=evelyn::patch::operations=debug evelyn build --dry-run
```

## Packages

### `Package`

```ron
Package("/system_ext/priv-app/Settings/Settings.apk", Signed, [ /* … */ ])
```

`Package(path, Signed | Unsigned, [operations])` opens an APK or JAR, applies
operations inside it, and rebuilds it. See [Package edits](./packages.md).

## Bindings and control flow

### `Let`

```ron
Let("vndk", Property(Partition("vendor"), "ro.vndk.version"))
```

`Let(name, value)` binds `{name}` for the operations after it in the same list.
If the value is not set, those operations are skipped and the skip is reported.
See [Recipes](./recipes.md#binding-values-let).

### `When`

```ron
When(Matches(Property(All, "ro.build.version.release"), r"^1[5-9]$"), [ /* … */ ])
```

`When(condition, [operations])` applies operations if the condition holds.
A condition that does not hold is not reported.

### `ForEach`

```ron
ForEach(Build("/product/app/*/oat"), "dir", [Remove("{dir}")])
```

`ForEach(over, name, [operations])` applies operations once for each item,
with `{name}` bound to it. `over` is one of these:

- a list of strings, where each string is bound
- a selection of files, where each file's device path is bound

A `ForEach` over nothing is reported. When its operations report the same
thing for every item, for example "nothing to remove", it is reported once,
not once per item.

## Checks

### `Assert`

```ron
Assert(Exists(Build("/vendor/etc/fstab.qcom")), "expected a Qualcomm vendor")
```

`Assert(condition, message)` fails the build with `message` if the condition
does not hold.

### `AssertHash`

```ron
AssertHash("/system/framework/services.jar", "<64 hex digits>")
```

`AssertHash(path, sha256)` fails the build if the file's content does not have
this SHA-256.

## Kit operations

[Kits](./kits.md) define more operations out of the built-in ones. You call them
the same way, for example `DisableFeature("oplus.software.ota")`.
