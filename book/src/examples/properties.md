# Set properties

<span class="filename">Filename: patches/properties/patch.ron</span>

```ron
[
    Step("build-props", [
        Props(All, [
            Set("ro.build.tags", "release-keys"),
            Set("persist.sys.disable_rescue", "true"),
            Remove("ro.oplus.ota.check"),
        ]),
    ]),
    Step("vendor-props", [
        Props(Partition("vendor"), [Set("persist.vendor.camera.privapp.list", "com.example.camera")]),
    ]),
    Step("model", [
        Let("model", Property(Partition("vendor"), "ro.product.vendor.model")),
        Props(All, [Set("ro.product.system.model", "{model}")]),
    ]),
]
```

- **`Props(All, …)`** edits every property file of the build, as init loads
  them. `Set` changes each file that defines the key. If none does, the key is
  added to the file init loads last, so the value takes effect.
- **`Props(Partition("vendor"), …)`** edits only the `vendor` partition's
  property files. A new key goes in `/vendor/build.prop`.
- **`Remove`** deletes every definition of a key in the scope. A key that no
  file defines is reported.
- **`Let` with `Property`** reads a value from the firmware, here the stock
  vendor's model name, and reuses it.

To edit exactly one file, scope `Props` to it:

```ron
Props(Build("/my_product/build.prop"), [Set("ro.example.flag", "1")])
```
