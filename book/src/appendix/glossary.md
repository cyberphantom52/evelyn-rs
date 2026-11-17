# Appendix A: Glossary

- **Asset**: A file in a patch's directory that its recipe names: a file to install, a diff, a smali method body, a TE policy.
- **Device repository**: A directory holding `evelyn.toml`, ports, patches and kits, which ports one ROM to one device.
- **Dry run**: `evelyn build --dry-run`: every recipe applied to the firmware, and no image built.
- **Fingerprint**: A build's `ro.build.fingerprint`. `evelyn.toml` pins the stock build by it.
- **Kit**: Operations defined out of the built-in ones, built into Evelyn or in a repository's `kits/`.
- **Location**: Where an expression looks for files: `Build`, `Rom`, `Stock`, `Asset`, `Package` or `Partition`.
- **Operation**: One change a recipe makes, such as `Remove`, `Props` or `Package`.
- **OTA package**: A full over-the-air update: a zip holding `payload.bin`, or `payload.bin` itself.
- **Patch**: A directory under `patches/` holding a recipe, `patch.ron`, and its assets.
- **Port**: One ROM version on the device: `ports/<name>.toml`.
- **Recipe**: A list of steps, each a name and a list of operations.
- **ROM**: The firmware being ported. It supplies the system side.
- **Stock firmware**: The device's own firmware. It supplies the vendor side.
- **`super`**: The partition holding the dynamic partitions, described by `[super]` in `evelyn.toml`.
- **System side**: `system`, `system_ext`, `product` and `system_dlkm`, and whatever other ROM partitions a port takes.
- **Treble**: Android's split between the system side and the vendor side.
- **Vendor side**: `vendor`, `odm`, `vendor_dlkm` and `odm_dlkm`.
