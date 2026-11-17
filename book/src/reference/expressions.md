# Expressions

Expressions are the values operations take. They appear in:

- `Let` values
- `When`, `Assert` and `Filter` conditions
- the list a `ForEach` goes over
- `Copy`'s `to`, `Install`'s destination and `Append`'s text

An expression evaluates to one of these:

- a **string**, which may be unset
- a **list** of strings
- a **selection** of files
- a **boolean**

## Locations

A location selects files, or names a partition. A location can be used
anywhere an expression is expected.

| Location            | Selects |
|---------------------|---------|
| `Build("/…")`       | What a device path, globs allowed, matches in the build so far. |
| `Rom("/…")`         | What a device path, globs allowed, matches in the ROM. |
| `Stock("/…")`       | What a device path, globs allowed, matches in the stock firmware. |
| `Asset("…")`        | A file or directory in the patch's directory. |
| `Package("/…")`     | Inside a `Package`, what a path from the package's root matches. |
| `Partition("name")` | A partition of the build. Use it with `Exists`. |

## Strings

| Expression                                   | Value |
|----------------------------------------------|-------|
| `"text"`, `r"text"`, `r#"text"#`             | A string. `{name}` in it is replaced with a bound value. |
| `Read(selection)`                            | The text of the one file the selection selects. |
| `Property(All, "key")`                       | The value the device ends up with, as init loads its property files. |
| `Property(Partition("vendor"), "key")`       | The value set by that partition's property files. |
| `Property(Build("/…/build.prop"), "key")`    | The value set in that one file. |
| `Join(list)`                                 | The list's strings, each followed by a newline. |

`Property` is unset if no file in its scope defines the key. The scopes are the
same as the scopes of [`Props`](./operations.md#props).

## Lists and selections

| Expression                     | Value |
|--------------------------------|-------|
| `Lines(string)`                | A string's lines, as a list. |
| `Partitions(Rom)`, `Partitions(Stock)` | The directories of the build's partitions that came from that firmware, such as `/system` and `/product`. |
| `Union([sel, sel, …])`         | Everything any of the selections selects, in order. |
| `Filter(over:, name:, keep:)`  | The items of a list or selection for which `keep` holds. Each item is bound as `{name}`: a string, or a file's device path. |

```ron
// Every permissions XML that mentions a feature.
Filter(
    over: Build("/*/etc/permissions/*.xml"),
    name: "file",
    keep: Contains(Read(Build("{file}")), "oplus.software.ota"),
)
```

## Conditions

| Expression                     | Holds when |
|--------------------------------|------------|
| `Exists(string)`               | The string is set. |
| `Exists(selection)`            | The selection is not empty. |
| `Exists(Partition("name"))`    | The build has that partition. |
| `Not(cond)`                    | `cond` does not hold. |
| `All([cond, …])`               | Every condition holds. |
| `Matches(string, "regex")`     | The string contains a match of the regular expression. The pattern is compiled when the recipe is loaded, and cannot use a bound value. |
| `Contains(string, "text")`     | The string contains the text. The text may use `{name}`. |

To require any one of several conditions, write `Not(All([Not(a), Not(b)]))`.

## Examples

```ron
// Apply operations only on Android 15 and later ROMs.
When(Matches(Property(All, "ro.build.version.sdk"), r"^(3[5-9]|[4-9][0-9])$"), [ /* … */ ])

// Bind the vendor's VNDK version.
Let("vndk", Property(Partition("vendor"), "ro.vndk.version"))

// Apply operations only when the stock firmware has a file.
When(Exists(Stock("/vendor/etc/permissions/android.hardware.nfc.xml")), [ /* … */ ])

// Remove an app from every partition the ROM supplies.
ForEach(Partitions(Rom), "p", [Remove("{p}/app/Facebook")])
```
