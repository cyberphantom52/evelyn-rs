# Enable and disable Oplus features

ColorOS and OxygenOS declare features in XML under each `my_*` partition. The
`oplus` kit, built into Evelyn, edits them.

<span class="filename">Filename: patches/features/patch.ron</span>

```ron
[
    Step("features", [
        EnableFeature(Oplus("oplus.software.example_feature")),
        EnableFeature(Android("android.hardware.example")),
        EnableFeature(App(
            name: "com.example.app.feature",
            file: "/my_product/etc/extension/com.oplus.app-features.xml",
            args: "boolean:true",
        )),
        DisableFeature("oplus.software.example_disabled"),
    ]),
]
```

- **`EnableFeature(Oplus(…))`** declares an `<oplus-feature>` in
  `/my_product/etc/permissions/oplus.feature.android.xml`, replacing any with
  the same name.
- **`EnableFeature(Android(…))`** declares a `<feature>` in
  `/my_product/etc/permissions/com.oplus.android-features.xml`.
- **`EnableFeature(App(…))`** declares an `<app_feature>` in the given
  extension file, with `args` if given.
- **`DisableFeature`** removes a feature of any kind from every permissions and
  extension XML that mentions it. A feature declared nowhere is reported.

`Oplus` and `Android` also take a `file:` to declare the feature somewhere
else:

```ron
EnableFeature(Oplus(name: "oplus.software.example_feature", file: "/my_stock/etc/permissions/example.xml"))
```

The port must take the `my_*` partitions from the ROM:

<span class="filename">Filename: ports/a15.toml</span>

```toml
partitions = ["my_*"]
```
