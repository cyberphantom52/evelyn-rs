# Edit a permissions XML

<span class="filename">Filename: patches/camera-permissions/patch.ron</span>

```ron
[
    Step("privapp-permissions", [
        Xml("/system_ext/etc/permissions/privapp-permissions-camera.xml", [
            PutChild(
                parent: "permissions/privapp-permissions[@package='com.example.camera']",
                key: "permission[@name='android.permission.READ_LOGS']",
                element: r#"<permission name="android.permission.READ_LOGS"/>"#,
                create: true,
            ),
            Remove(path: "//deny-permission[@name='android.permission.CAMERA']"),
        ]),
    ]),
]
```

- **`Xml(path, [ … ])`** opens a file as XML and applies its edits in order.
- **`PutChild`** puts `element` under every match of `parent`, in place of the
  children `key` matches. With `create: true`, it appends the element where
  none matches, so the permission is granted whether or not it was before.
- **`Remove`** removes every match. A path that starts with `//` matches at
  any depth.

Paths name elements from the document element down. Selectors take
`[@attr='value']`, `[text()='value']` and `[child='value']`. `SetText` and
`SetAttribute` change text and attributes.
[XML edits](../reference/xml.md) lists them all.
