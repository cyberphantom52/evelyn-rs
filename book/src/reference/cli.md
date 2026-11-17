# Command line

```text
evelyn <COMMAND>

Commands:
  init     Set up a device repository, and check what builds need
  build    Build the device repository into images, a super image, or a ZIP
  extract  Replace an OTA package in firmware/ by its images
  check    Download and check what builds need, and generate the signing key
```

Every command prints its result as JSON to stdout, and logs to stderr. On
failure, it prints the error and each of its causes, then exits with status 1:

```text
error: <what Evelyn was doing>
  caused by: <why it failed>
  caused by: <the cause of that>, ...
```

## `evelyn init`

```text
evelyn init [DIR]
```

Creates a device repository at `DIR`, or in the current directory if `DIR` is
not given. It writes:

- `evelyn.toml`
- `ports/`, `patches/`, `firmware/stock/` and `firmware/rom/`
- `.gitignore` entries for `/.evelyn/`, `/firmware/` and `/out/`

`evelyn.toml` starts with an empty `product_id` and `fingerprint`, and a
commented-out `[super]` table, for you to fill in.

It then runs [`check`](#evelyn-check). If `DIR` already holds an `evelyn.toml`,
`init` fails.

## `evelyn build`

```text
evelyn build [OPTIONS]
```

Builds the device repository that contains the current directory.

| Option                    | Default                         | Meaning |
|---------------------------|---------------------------------|---------|
| `--rom <SOURCE>`          | `firmware/rom`                  | The ROM. |
| `--stock <SOURCE>`        | `firmware/stock`                | The device's stock firmware. |
| `--output <KIND>`         | `super` with a super layout, else `partitions` | `partitions`, `super` or `zip`. |
| `--output-dir <DIR>`      | `out/<port>`                    | Where images or the ZIP are written. |
| `--work-dir <DIR>`        | `.evelyn`                       | The build's working space. |
| `--patch <DIR>`           | none                            | An extra patch directory, applied after the port's patches. Can be given more than once. |
| `--dry-run`               | off                             | Apply every recipe, write no images, and print what was composed. |
| `--debug`                 | off                             | Make a [debug build](../guides/debug.md). |

A `SOURCE` is one of these:

- a directory of `<partition>.img` images
- an OTA package file
- an `http(s)://` URL of an OTA package

For a URL, only the partitions the build extracts are downloaded.

## `evelyn extract`

```text
evelyn extract [rom|stock]
```

Replaces the OTA package in `firmware/rom` or `firmware/stock` with all of its
images. With no argument, it does both. After this, builds read the images in
place instead of extracting partitions from the package each time. Images left
there by an earlier extraction are replaced.

It prints the list of images it wrote.

## `evelyn check`

```text
evelyn check
```

Sets up everything builds need that is kept outside the `evelyn` binary. It:

- downloads every pinned file Evelyn lacks
- checks every pinned file it already has against its SHA-256
- generates the APK signing key if there is none

It prints each item's `name`, `path`, `state` (`present`, `new` or `failed`)
and `error`. If any item fails, `check` exits with an error.
