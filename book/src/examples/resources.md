# Change an app's resources

<span class="filename">Filename: patches/settings-resources/patch.ron</span>

```ron
[
    Step("settings-resources", [
        Package("/system_ext/priv-app/Settings/Settings.apk", [
            Resources([
                Set(type: "bool", name: "config_show_example", config: "", value: "false"),
                Set(type: "string", name: "device_name", config: "", value: "Pong"),
                Set(type: "string", name: "device_name", config: "en-rIN", value: "Pong", create: true),
                Set(type: "integer-array", name: "example_refresh_rates", config: "", value: ["60", "90", "120"]),
            ]),
        ]),
        Sign("/system_ext/priv-app/Settings/Settings.apk"),
    ]),
]
```

- **`Resources([ … ])`** sets the package's values resources by type, name and
  configuration. It works only inside a `Package`.
- **`config`** is `""` for the default `values/`, or a qualifier: `"en-rIN"`
  is `values-en-rIN/`.
- **`value`** is what goes between the tags. An array takes a list of its
  items.
- A resource the configuration does not have is an error, unless
  **`create: true`** adds it.

The types are `string`, `bool`, `integer`, `color`, `dimen`, `fraction`,
`string-array` and `integer-array`.

Other resource files are text, from the package's root:

```ron
Package("/system_ext/priv-app/Settings/Settings.apk", [
    Xml("/res/xml/example_preferences.xml", [
        Remove(path: "//Preference[@{http://schemas.android.com/apk/res/android}key='example']"),
    ]),
])
Sign("/system_ext/priv-app/Settings/Settings.apk")
```
