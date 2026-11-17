# Apply a patch to some builds only

A port picks ROM builds as a whole. Within a patch, `When` and `Let` pick
which operations apply.

<span class="filename">Filename: patches/refresh-rate/patch.ron</span>

```ron
[
    Step("refresh-rate", [
        Assert(Exists(Build("/vendor/etc/display/display_config.xml")), "expected the vendor display config"),
        When(Matches(Property(All, "ro.build.version.incremental"), r"^V1[5-9]\."), [
            Props(All, [Set("persist.vendor.example.refresh_rate", "120")]),
        ]),
        When(Exists(Partition("my_bigball")), [
            ForEach(Partitions(Rom), "p", [Remove("{p}/app/ExampleApp")]),
        ]),
    ]),
    Step("vndk-only", [
        Let("vndk", Property(Partition("vendor"), "ro.vndk.version")),
        Props(Partition("vendor"), [Set("ro.example.vndk", "{vndk}")]),
    ]),
]
```

- **`Assert`** fails the build with its message when the firmware is not what
  the patch expects. Use it when going on would be wrong.
- **`When`** applies operations only if the condition holds, and reports
  nothing when it does not.
- **`Let`** skips the rest of its list when the value is not set, and reports
  the skip. Here, a vendor without `ro.vndk.version` skips `vndk-only`.

The pattern of a `Matches` is a regular expression, compiled when the recipe
is loaded. It is not substituted, so it cannot use a bound value, and a
quantifier is written as usual:

```ron
When(Matches(Property(All, "ro.build.id"), r"^[A-Z]{4}\."), [ ])
```
