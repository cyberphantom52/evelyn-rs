# Share operations with a kit

Several patches doing the same thing can share an operation, defined in the
repository's `kits/`:

<span class="filename">Filename: kits/privapp.ron</span>

```ron
Kit(
    operations: {
        "GrantPrivapp": (
            parameters: ["package", "permission"],
            defaults: {"file": "/system_ext/etc/permissions/privapp-permissions-extra.xml"},
            operations: [
                When(Not(Contains(Read(Build("{file}")), r#"package="{package}""#)), [
                    Xml("{file}", [
                        PutChild(
                            parent: "permissions",
                            key: "privapp-permissions[@package='{package}']",
                            element: r#"<privapp-permissions package="{package}"/>"#,
                            create: true,
                        ),
                    ]),
                ]),
                Xml("{file}", [
                    PutChild(
                        parent: "permissions/privapp-permissions[@package='{package}']",
                        key: "permission[@name='{permission}']",
                        element: r#"<permission name="{permission}"/>"#,
                        create: true,
                    ),
                ]),
            ],
        ),
    },
)
```

<span class="filename">Filename: patches/camera-permissions/patch.ron</span>

```ron
[
    Step("camera-permissions", [
        Install("privapp-permissions-extra.xml", "/system_ext/etc/permissions/privapp-permissions-extra.xml"),
        GrantPrivapp(package: "com.example.camera", permission: "android.permission.READ_LOGS"),
        GrantPrivapp(package: "com.example.gallery", permission: "android.permission.MANAGE_MEDIA"),
    ]),
]
```

<span class="filename">Filename: patches/camera-permissions/privapp-permissions-extra.xml</span>

```xml
<?xml version="1.0" encoding="utf-8"?>
<permissions>
</permissions>
```

- **`parameters`** must be given by every call. A call to an operation with
  only one parameter can give it bare: `Operation("value")`.
- **`defaults`** may be given by name. A call that leaves one out gets its
  default.
- **`operations`** are built-in operations, with each parameter bound as
  `{name}`.

The `When` adds the package's `<privapp-permissions>` element only if the file
does not have one yet. `PutChild` replaces the children its `key` matches, so
without the guard a second call for the same package would drop the
permissions the first call granted.

An asset a kit's operation names is the calling patch's. A kit's operation
cannot call another kit's, or reuse a name another operation has.
