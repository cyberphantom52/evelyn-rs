# Package edits

`Package(path, Signed | Unsigned, [operations])` opens an APK or JAR, applies
operations inside it, and rebuilds it.

```ron
Package("/system_ext/priv-app/Settings/Settings.apk", Signed, [
    StubMethod(
        class: "Lcom/android/settings/Utils;",
        method: "isOtaAvailable()Z",
        returns: Zero,
    ),
    Resources([
        Set(type: "string", name: "device_name", config: "", value: "Pong"),
    ]),
])
```

| Argument   | Meaning |
|------------|---------|
| path       | The package's device path. It can use `{name}`. |
| `Signed`   | Sign the rebuilt package with Evelyn's key. An APK that Android verifies needs a valid signature once its contents change. |
| `Unsigned` | Leave the rebuilt package unsigned, for a package Android does not verify. |
| operations | Operations applied inside the package. |

## Paths inside a package

Paths inside a package start from its root, as the package has them. For
example, `/assets/…`, `/lib/arm64-v8a/…` and `/classes.dex`.

Resources and the manifest are exposed as **text**:

- `/AndroidManifest.xml`
- `/res/values/strings.xml`
- `/res/layout/…`, and so on

Only what the operations change is put back. A new file under `/res/` adds its
resource. A removed file removes its resource.

## Operations allowed inside a package

- the file operations: `Copy`, `Install` and `Remove`
- the document operations: `Text`, `Bytes` and `Xml`
- `Let`, `When` and `ForEach`
- `Assert` and `AssertHash`
- `Resources` and the smali edits, which work only inside a package

## `Resources`

```ron
Resources([
    Set(type: "bool", name: "config_show_ota", config: "", value: "false"),
    Set(type: "string", name: "app_name", config: "en-rIN", value: "Settings", create: true),
    Set(type: "integer-array", name: "refresh_rates", config: "", value: ["60", "90", "120"]),
])
```

`Resources([Set(…)])` sets the package's values resources. It edits them by
type, name and configuration, not by file.

| Field    | Meaning |
|----------|---------|
| `type`   | `string`, `bool`, `integer`, `color`, `dimen`, `fraction`, `string-array` or `integer-array`. |
| `name`   | The resource's name. |
| `config` | The configuration: `""` for the default `values/`, or a qualifier like `"en-rIN"` for `values-en-rIN/`. |
| `value`  | What goes between the tags. For an array, a list of its items. |
| `create` | Default `false`. If the configuration does not have the resource, add it instead of failing. |

## Smali edits

A package's code is edited as smali, in place. Classes are written in type
descriptor form, such as `Lcom/example/Foo;`. Methods are written as name plus
signature, such as `isEnabled()Z`.

### `StubMethod(class:, method:, returns:)`

Replaces the method's body so that it only returns a value:

| `returns` | Returns |
|-----------|---------|
| `Void`    | nothing (`V` methods) |
| `Zero`    | `0`, `false` or `null` |
| `One`     | `1` or `true` |

### `ReplaceMethod(class:, method:, body:)`

Replaces the method with the smali in the asset `body`.

### `InsertMethod(class:, method:, body:)`

Adds the asset `body` to the class as a new method.

### `ReplaceOperand(from:, to:, class:, method:)`

Replaces the operand written as `from` with `to` in every instruction that has
it. The operand can be:

- a method, such as `Lcom/example/Name;->get()I`
- a field
- a type
- a string, written in its quotes
- a number

The scope is every class, or only `class:`, or only its `method:`. At least one
instruction in that scope must have the operand, or the build fails.

```ron
ReplaceOperand(
    from: r#""https://ota.example.com""#,
    to: r#""""#,
    class: "Lcom/oplus/ota/Config;",
)
```
