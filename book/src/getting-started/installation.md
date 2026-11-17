# Installation

## Requirements

- **Rust.** The workspace uses edition 2024. See `rust-version` in the root
  `Cargo.toml` for the minimum toolchain.
- **A C and C++ compiler.** Some dependencies build native code: the SELinux
  policy library (C++), and the xz, zstd and `ring` libraries (C).

## Build from source

```console
$ git clone https://github.com/cyberphantom52/evelyn-rs
$ cd evelyn-rs
$ cargo build --release
```

The binary is written to `target/release/evelyn`. Put it on your `PATH`, or
install it with Cargo:

```console
$ cargo install --path crates/evelyn
```

## The data directory

Some files a build needs are not part of the `evelyn` binary. Evelyn keeps them
in its data directory:

| Platform | Data directory                           |
|----------|------------------------------------------|
| macOS    | `~/Library/Application Support/evelyn`   |
| Linux    | `~/.local/share/evelyn`                  |

The data directory holds:

- **VNDK APEXes.** Each one is downloaded the first time a build needs it, then
  checked against the SHA-256 that Evelyn pins.
- **Android's policy macros.** `te_macros` and `global_macros` come from AOSP's
  `system/sepolicy` at a pinned commit. They are downloaded the first time a
  patch adds a TE policy, and checked the same way.
- **The APK signing key.** It is generated the first time it is needed. Evelyn
  uses it to sign the packages that recipes rebuild with `Package(…, Signed, …)`.

## `evelyn check`

To set all of this up at once, run:

```console
$ evelyn check
```

`check` does three things:

- downloads every pinned file that is missing
- verifies every pinned file already there
- generates the signing key if there is none

It prints one JSON entry for each item, with its state: `present`, `new` or
`failed`. `evelyn init` runs the same check after it creates a repository.

```json
[
  {
    "name": "...",
    "path": "...",
    "state": "present",
    "error": null
  }
]
```

If any item fails, `check` exits with an error after printing the list.

## Logging

Evelyn logs to stderr at the `info` level. It prints its results as JSON to
stdout. Set `RUST_LOG` to change the log level:

```console
$ RUST_LOG=debug evelyn build
```
