# Stub a method in an APK

<span class="filename">Filename: patches/no-ota-check/patch.ron</span>

```ron
[
    Step("no-ota-check", [
        Package("/system_ext/priv-app/Settings/Settings.apk", Signed, [
            StubMethod(
                class: "Lcom/example/settings/UpdateChecker;",
                method: "isUpdateAvailable()Z",
                returns: Zero,
            ),
        ]),
    ]),
]
```

- **`Package(path, Signed, [ … ])`** opens the APK, applies the operations
  inside it, and rebuilds it. `Signed` signs it with Evelyn's key, which an APK
  Android verifies needs once its contents change.
- **`StubMethod`** replaces the method's body so it only returns. `Zero`
  returns `0`, `false` or `null`; `One` returns `1` or `true`; `Void` returns
  nothing.

Classes are type descriptors, `Lcom/example/Foo;`, and methods are a name and
a signature, `isUpdateAvailable()Z`. Both are as `baksmali` prints them.

To replace a method with your own smali, give its body as an asset:

```ron
ReplaceMethod(
    class: "Lcom/example/settings/UpdateChecker;",
    method: "isUpdateAvailable()Z",
    body: "smali/isUpdateAvailable.smali",
)
```

To change what instructions refer to, in every class or in one:

```ron
ReplaceOperand(
    from: r#""https://ota.example.com/check""#,
    to: r#""""#,
    class: "Lcom/example/settings/UpdateChecker;",
)
```
