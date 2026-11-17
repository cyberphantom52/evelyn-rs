# Kits

A **kit** defines new operations out of the
[built-in ones](./operations.md). Kits come from two places:

- **Evelyn's kits**, in `assets/kits/`, are compiled into Evelyn. They cover
  ROM families.
- **A device repository's kits**, in `kits/*.ron`, are for what several of
  that repository's patches do in the same way.

Every recipe can call any kit's operation, from either place, the same way it
calls a built-in one:

- An operation with **one required parameter** takes it bare:
  `DisableFeature("oplus.software.ota")`.
- An operation with **several parameters**, or with defaults, takes them by
  name: `EnableFeature(App(name: "…", file: "/…"))`.

## Writing a kit

<span class="filename">Filename: kits/labels.ron</span>

```ron
Kit(
    operations: {
        "Labels": (
            parameters: ["service", "exec"],
            operations: [
                Text("/vendor/etc/selinux/vendor_service_contexts", [Append("{service}")]),
                Text("/vendor/etc/selinux/vendor_file_contexts", [Append("{exec}")]),
            ],
        ),
    },
)
```

A patch then calls it like this:

<span class="filename">Filename: patches/camera-hal/patch.ron</span>

```ron
[
    Step("labels", [
        Labels(
            service: "vendor.oplus.camera u:object_r:hal_camera_service:s0",
            exec: r"/odm/bin/hw/vendor\.oplus\.camera u:object_r:hal_camera_default_exec:s0",
        ),
    ]),
]
```

Each operation in a kit has these fields:

| Field        | Meaning |
|--------------|---------|
| `parameters` | The parameters a call must give. |
| `defaults`   | Optional. Parameters a call may give, with the value each takes when the call does not give it. |
| `operations` | Built-in operations, with each parameter bound as `{name}`. |

Rules for kit operations:

- A kit operation runs its operations the way a step does. If they skip
  something, the rest of the calling step still applies.
- A kit operation can only use built-in operations. It cannot call another
  kit's operation.
- An asset that a kit operation names belongs to the **calling** patch. The kit
  has no directory of its own.
- An operation can be named `Outer` or `Outer(Inner)`. The second form is
  called as `Outer(Inner(…))`.
- A kit operation cannot use the name of a built-in operation, or of an
  operation another kit already defines, including one of Evelyn's kits.

## Property files

A kit can list, in `property_files`, the property files its ROM family's init
loads after Android's own, in the order it loads them. Each entry must be a
device path to a `.prop` file. That order decides two things:

- what `Property(All, …)` reads
- where `Props(All, [Set(…)])` adds a key that no file defines

If a property file outside this order gives a key that is read a different
value, Evelyn reports it.

## `oplus`: ColorOS and OxygenOS

### Property files

The kit lists each `my_*` partition's `build.prop`: `my_product`, `my_region`,
`my_stock`, `my_heytap`, `my_carrier`, `my_company`, `my_preload`,
`my_bigball`, `my_engineering` and `my_manifest`. Values in these files win
over Android's.

### `EnableFeature`

Declares a feature. If a feature with that name is already declared, it is
replaced.

```ron
// <feature name=…> in /my_product/etc/permissions/com.oplus.android-features.xml
EnableFeature(Android("android.hardware.camera.concurrent"))

// <oplus-feature name=…> in /my_product/etc/permissions/oplus.feature.android.xml
EnableFeature(Oplus("oplus.software.display.refresh_rate"))

// The same, in a different file.
EnableFeature(Oplus(name: "oplus.software.foo", file: "/my_stock/etc/permissions/foo.xml"))

// <app_feature name=… args=…> in an extension file.
EnableFeature(App(name: "com.oplus.camera.feature", file: "/my_product/etc/extension/com.oplus.app-features.xml", args: "boolean:true"))
```

| Form      | Parameters | Default `file` |
|-----------|------------|----------------|
| `Android` | `name`, `file` | `/my_product/etc/permissions/com.oplus.android-features.xml` |
| `Oplus`   | `name`, `file` | `/my_product/etc/permissions/oplus.feature.android.xml` |
| `App`     | `name`, `file` (required), `args` | none |

### `DisableFeature`

```ron
DisableFeature("oplus.software.ota")
```

Removes a feature of any kind from every permissions and extension XML that
mentions it: `/*/etc/permissions/*.xml` and `/*/etc/extension/*.xml`. Files that
do not mention it are not parsed. A feature declared nowhere is reported.
