# Recipes

A recipe is the `patch.ron` of a patch, written in
[RON](https://github.com/ron-rs/ron). It is a list of steps. Each step has a
name and a list of operations:

<span class="filename">Filename: assets/recipes/vndk.ron</span>

```ron
[
    Step("vndk-apex", [
        Let("vndk", Property(Partition("vendor"), "ro.vndk.version")),
        When(Not(Exists(Build("/system_ext/apex/com.android.vndk.v{vndk}.apex"))), [
            Remove("/system_ext/apex/com.android.vndk*.apex"),
            Install("apex/com.android.vndk.v{vndk}.apex", "/system_ext/apex/com.android.vndk.v{vndk}.apex"),
        ]),
    ]),
]
```

Evelyn applies this recipe to every build, to give a vendor older than Android
15 the VNDK APEX it needs.

## Steps

`Step(name, [operations])`

- Steps are applied in the order they are written.
- A step's operations are applied in order. Each operation sees what the
  operations before it changed.
- The step's name appears in reports and in errors. An error says which recipe
  and which step it came from.

## Binding values: `Let`

`Let(name, value)` binds a name to a string read from the build so far. The
operations after it in the same list can use the value as `{name}`:

```ron
Step("product-device", [
    Let("device", Property(Partition("vendor"), "ro.product.vendor.device")),
    Props(All, [Set("ro.product.system.device", "{device}")]),
]),
```

- A binding applies only to the operations **after** it, in the **same list**.
  A `Let` inside a `When` or a `ForEach` is not seen outside it.
- If the value is not set, the operations after the `Let` are skipped, and the
  skip is reported. For example, a property that no file defines is not set.
- The value must be made of `[A-Za-z0-9._-]`, or the build fails. Bound values
  come from firmware and go into paths, so free text is not allowed.

## Conditions: `When`

`When(condition, [operations])` applies operations only if the condition
holds. A condition that does not hold is not reported. To make a skip visible,
use `Assert` instead. See [Expressions](./expressions.md#conditions) for the
conditions you can write.

## Strings and `{name}`

Any string in an operation can use a bound value as `{name}`. This includes
paths, property values, XML selectors and asset names. A name is bound by:

- `Let`
- `ForEach` and `Filter`
- a kit operation's parameters

The rules for writing them:

- A name is made of `[A-Za-z0-9_-]` and starts with a letter or a digit.
- `{{` and `}}` stand for a literal `{` and `}`.
- Any other brace stands for itself. This means an XML namespace like
  `{http://schemas.android.com/apk/res/android}name` needs no escaping.

### Raw strings

Write a regular expression, or a string full of quotes, as a RON raw string:

```ron
Matches(Read(Build("/vendor/build.prop")), r"ro\.vndk\.version=3[0-4]")
PutChild(parent: "permissions", key: "feature[@name='x']", element: r#"<feature name="x"/>"#, create: true)
```

## Paths

- **Device paths** always start with `/`, and are written as the device sees
  them: `/vendor/etc/fstab.qcom`. Evelyn resolves each one to a partition by
  mount point. On system-as-root devices, `system` is mounted at `/`.
- **Globs** are allowed wherever an operation matches files.
- **Asset paths** are relative to the patch's directory. This is also true when
  a [kit](./kits.md) operation names an asset: it is the calling patch's
  asset.
- **Inside a `Package`**, paths start from the package's root:
  `/res/values/strings.xml`.

### Firmware paths and bound values

A build from an OTA package extracts only the partitions its recipes read. It
finds them from the `Rom("/…")` and `Stock("/…")` paths written out in full. A
path that uses a bound value, such as `Rom("/odm/bin/{name}")`, is not known
until the recipe is applied, so it does not make the build extract its
partition. Either name another path in the same partition in full, or replace
the package with its images using `evelyn extract`.

To act on the same path in several partitions, loop over them:

```ron
ForEach(Partitions(Rom), "p", [Remove("{p}/app/Facebook")])
```

## RON conveniences

Evelyn parses recipes with two RON extensions turned on:

- **Implicit `Some`.** You write an optional field as a plain value:
  `class: "Lcom/example/Foo;"`, not `class: Some("Lcom/example/Foo;")`.
- **Unwrapped newtypes.** A variant that wraps one value takes it directly. For
  example, `Import(Rom("/…"))`.

## What is checked, and when

Evelyn checks a recipe's syntax when it loads the recipe. It checks everything
else when the operation runs. A path that matches nothing, or a value of the
wrong form, fails the build at that step.

Some operations report a problem instead of failing. Their entries in the
reference say so. One example is `Remove` of a path that matches nothing.
