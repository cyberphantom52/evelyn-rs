# Patch bytes in a binary

<span class="filename">Filename: patches/libfoo-check/patch.ron</span>

```ron
[
    Step("libfoo-check", [
        AssertHash("/vendor/lib64/libfoo.so", "3f1c4e2a9b7d6c5e8f0a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60"),
        Bytes("/vendor/lib64/libfoo.so", {
            487572: "1f2003d5",
            487576: "1f2003d5",
        }),
    ]),
]
```

- **`AssertHash`** fails the build unless the file has exactly this SHA-256.
  An offset only means something in one exact build of a file, so the hash
  check comes first.
- **`Bytes(path, { offset: "hex" })`** overwrites bytes at each offset.
  Offsets are decimal integers. The bytes are hex, two digits each. Here,
  `1f2003d5` is an AArch64 `nop`.

When the stock firmware is updated, the hash no longer matches and the build
fails at this step, instead of writing bytes into the wrong place.
