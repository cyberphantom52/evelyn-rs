# Patches and recipes

A **patch** is a directory directly under `patches/`. It is named for what it
changes, and it holds:

- `patch.ron`: the **recipe**
- any **assets** the recipe uses: files to install, diffs, smali method
  bodies, SELinux policy

```text
patches/disable-ota-updater/
├── patch.ron
└── smali/
    └── isUpdateAvailable.smali
```

Patches cannot be nested. A patch is always `patches/<name>`.

When several patches do the same kind of thing, put that shared logic in a
[kit](../reference/kits.md) in the repository's `kits/` directory. Each patch
then calls the kit's operation.

## Recipes

A recipe is a list of **steps**. Each step is a list of **operations**:

```ron
[
    Step("remove-bloat", [
        ForEach(Partitions(Rom), "p", [Remove("{p}/app/Facebook*")]),
    ]),
    Step("build-tag", [
        Props(All, [Set("ro.build.tags", "release-keys")]),
    ]),
]
```

Steps are applied in the order they are written. A step's operations are
applied in order too, and each one sees what the operations before it changed.
Within a step:

- **`Let(name, value)`** binds a name to a value read from the build so far.
  The operations after it can use the value as `{name}`. If the value is not
  set, those operations are skipped, and the build reports the skip.
- **`When(condition, [operations])`** applies operations only if the condition
  holds.

Some operations open a file as a document of one kind and apply edits to it:
`Props`, `Text`, `Bytes`, `Xml` and `Policy`.

## Checked when applied

Evelyn checks a recipe's syntax before it applies it, and nothing else. A wrong
path or a wrong value fails the build at the step that uses it.

`Assert` and `AssertHash` fail the build when a file is not what the patch expects:

```ron
Step("check-services", [
    AssertHash("/system/framework/services.jar", "<64 hex digits>"),
]),
```

## Built-in recipes

After the port's patches, every build applies recipes that ship with Evelyn:

- **VNDK APEX.** A vendor older than Android 15 needs the VNDK APEX matching
  its `ro.vndk.version`. If the build does not have it, Evelyn installs it.
- **Debug.** With `--debug`, Evelyn applies one more recipe after all the
  others. See [Debug builds](../guides/debug.md).
- **Permissive SELinux.** With `--selinux-permissive`, Evelyn applies one
  more recipe after all the others. See
  [Permissive SELinux](../guides/debug.md#permissive-selinux).
