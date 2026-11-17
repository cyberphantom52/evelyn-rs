# Building in CI

A build can read its firmware from URLs. Of a remote OTA package, Evelyn
downloads only the bytes of the partitions it extracts, so a CI job needs no
firmware checked in and does not download whole OTAs:

```console
$ evelyn build \
  --rom   https://example.com/rom-ota.zip \
  --stock https://example.com/stock-ota.zip \
  --output zip
```

Each extracted partition is checked against the hash recorded in the package's
manifest.

## GitHub Actions

A workflow for a device repository:

```yaml
name: Build
on:
  workflow_dispatch:
    inputs:
      rom:
        description: ROM OTA URL
        required: true
      stock:
        description: Stock OTA URL
        required: true

jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Install Evelyn
        run: cargo install --locked --git https://github.com/cyberphantom52/evelyn-rs evelyn

      - name: Set up Evelyn's data directory
        run: evelyn check

      - name: Build
        run: evelyn build --rom "${{ inputs.rom }}" --stock "${{ inputs.stock }}" --output zip

      - uses: actions/upload-artifact@v4
        with:
          name: port
          path: out/
```

## Notes

- **Cache the data directory.** On Linux, this is `~/.local/share/evelyn`. It
  holds the downloaded VNDK APEXes and the signing key.
- **Keep the signing key stable.** If CI generates a new key on every run, every
  build signs its rebuilt APKs with a different key. Then an APK signed by one
  build cannot be updated by the next. Restore the same key from a secret or a
  cache.
- **Use `--dry-run` as a check.** Run it on every push to a device repository.
  It catches patches that no longer apply to the firmware, without building
  images.
