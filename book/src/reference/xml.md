# XML edits

`Xml(path, [edits])` applies a list of edits to one XML file, in order.
Each edit's paths, selectors, names and values are strings, so they can use
`{name}`.

```ron
Xml("/my_product/etc/permissions/oplus.feature.android.xml", [
    PutChild(
        parent: "oplus-config",
        key: "oplus-feature[@name='oplus.software.display.refresh_rate']",
        element: r#"<oplus-feature name="oplus.software.display.refresh_rate"/>"#,
        create: true,
    ),
    Remove(path: "//oplus-feature[@name='oplus.software.ota']"),
])
```

## Paths

A path lists element selectors from the document element down, separated by
`/`:

```text
resources/string[@name='app_name']
```

- A step after `//` matches at any depth below, not only among direct
  children. For example, `//feature[@name='x']`.
- A `/` inside a predicate or inside a `{namespace}` is part of that predicate
  or namespace.

### Selectors

| Selector                 | Matches |
|--------------------------|---------|
| `name`                   | An element named `name`. |
| `*`                      | Any element. |
| `name[@attr]`            | An element with the attribute. |
| `name[@attr='value']`    | An element whose attribute has the value. |
| `name[text()='value']`   | An element whose text is the value. |
| `name[child='value']`    | An element with a child element whose text is the value. |

Names can carry a namespace as `{uri}local`. For example,
`{http://schemas.android.com/apk/res/android}name`.

## Edits

### `SetText(path:, value:)`

Replaces the text of every element that `path` matches.

### `SetAttribute(path:, name:, value:, create:)`

Sets the attribute `name` to `value` on every match that has it. With
`create: true`, it also adds the attribute to matches that do not have it.

### `Remove(path:)`

Removes every match.

### `PutChild(parent:, key:, element:, create:)`

Under every match of `parent`, replaces the children that `key` matches with
`element`. `element` is one XML element, written out as text. A raw string,
`r#"<feature name="x"/>"#`, saves escaping its quotes. With
`create: true`, it appends the element where no child matches.

`PutChild` with `create: true` is how you say "declare this, replacing any
existing declaration". Applying it twice gives the same result as applying it
once.
