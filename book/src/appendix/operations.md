# Appendix B: Operation index

| Operation        | Form | Inside `Package` | Inside `BootImage` | Reference |
|------------------|------|:---:|:---:|-----------|
| `Copy`           | `Copy(from:, to:, metadata:, overwrite:, create_parent:)` | ✓ | ✓ | [Files](../reference/operations.md#copy) |
| `Import`         | `Import(Rom("/…"))` | | ✓ | [Files](../reference/operations.md#import) |
| `Install`        | `Install(asset, to)` | ✓ | ✓ | [Files](../reference/operations.md#install) |
| `Remove`         | `Remove("/…")` | ✓ | ✓ | [Files](../reference/operations.md#remove) |
| `SetMetadata`    | `SetMetadata(pattern:, uid:, gid:, mode:, selinux:)` | | ✓ | [Files](../reference/operations.md#setmetadata) |
| `Props`          | `Props(All \| Partition(…) \| | Build(…), [Set(key, value), Remove(key)])` | | [Properties](../reference/operations.md#props) |
| `Text`           | `Text(path, [Append(text), Diff(asset)])` | ✓ | ✓ | [File contents](../reference/operations.md#text) |
| `Bytes`          | `Bytes(path, {offset: "hex"})` | ✓ | ✓ | [File contents](../reference/operations.md#bytes) |
| `Xml`            | `Xml(path, [SetText, SetAttribute, Remove, PutChild])` | ✓ | ✓ | [XML edits](../reference/xml.md) |
| `Policy`         | `Policy(path, [Add(asset), Import(types:, from:)])` | | | [SELinux](../reference/operations.md#policy) |
| `Package`        | `Package(path, [ … ])` | | | [Package edits](../reference/packages.md) |
| `BootImage`      | `BootImage(name, [ … ])` | | | [Boot images](../reference/operations.md#bootimage) |
| `Sign`           | `Sign(path)` | | | [Signing](../reference/packages.md#signing) |
| `Resources`      | `Resources([Set(type:, name:, config:, value:, create:)])` | only | | [Package edits](../reference/packages.md#resources) |
| `StubMethod`     | `StubMethod(class:, method:, returns:)` | only | | [Package edits](../reference/packages.md#stubmethodclass-method-returns) |
| `ReplaceMethod`  | `ReplaceMethod(class:, method:, body:)` | only | | [Package edits](../reference/packages.md#replacemethodclass-method-body) |
| `InsertMethod`   | `InsertMethod(class:, method:, body:)` | only | | [Package edits](../reference/packages.md#insertmethodclass-method-body) |
| `ReplaceOperand` | `ReplaceOperand(from:, to:, class:, method:)` | only | | [Package edits](../reference/packages.md#replaceoperandfrom-to-class-method) |
| `Let`            | `Let(name, value)` | ✓ | ✓ | [Recipes](../reference/recipes.md#binding-values-let) |
| `When`           | `When(condition, [ … ])` | ✓ | ✓ | [Control flow](../reference/operations.md#when) |
| `ForEach`        | `ForEach(over, name, [ … ])` | ✓ | ✓ | [Control flow](../reference/operations.md#foreach) |
| `Assert`         | `Assert(condition, message)` | ✓ | ✓ | [Checks](../reference/operations.md#assert) |
| `AssertHash`     | `AssertHash(path, sha256)` | ✓ | ✓ | [Checks](../reference/operations.md#asserthash) |

## Kit operations

| Operation        | Kit | Reference |
|------------------|-----|-----------|
| `EnableFeature(Android(…))`, `EnableFeature(Oplus(…))`, `EnableFeature(App(…))` | `oplus` | [Kits](../reference/kits.md#enablefeature) |
| `DisableFeature(name)` | `oplus` | [Kits](../reference/kits.md#disablefeature) |
