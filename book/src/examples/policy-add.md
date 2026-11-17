# Add SELinux policy for a daemon

The daemon from [Install an init service](./init-service.md) runs in its own
domain. Its policy is a TE file written with Android's policy macros:

<span class="filename">Filename: patches/mydaemon/sepolicy/mydaemon.te</span>

```text
type mydaemon, domain;
type mydaemon_exec, exec_type, vendor_file_type, file_type;

init_daemon_domain(mydaemon)

allow mydaemon vendor_data_file:dir create_dir_perms;
allow mydaemon vendor_data_file:file create_file_perms;
```

<span class="filename">Filename: patches/mydaemon/patch.ron</span>

```ron
[
    Step("mydaemon", [
        Install("mydaemon", "/vendor/bin/mydaemon"),
        SetMetadata(pattern: "/vendor/bin/mydaemon", mode: 0o755, selinux: "u:object_r:mydaemon_exec:s0"),
        Install("mydaemon.rc", "/vendor/etc/init/mydaemon.rc"),
    ]),
    Step("mydaemon-policy", [
        Policy("/vendor/etc/selinux/vendor_sepolicy.cil", [Add("sepolicy/mydaemon.te")]),
        Text("/vendor/etc/selinux/vendor_file_contexts", [
            Append(r"/vendor/bin/mydaemon u:object_r:mydaemon_exec:s0"),
        ]),
    ]),
]
```

- **`Policy(path, [ … ])`** patches one of the vendor's CIL files.
- **`Add(asset)`** compiles the TE file and adds it. Several `Add`s in a row
  are added together, so one can use types another declares.
- The result must compile with the device's other policy files, as init
  compiles them at boot. A rule that names an undeclared type fails the build.
- **`Text` with `Append`** adds the file's label to `vendor_file_contexts`, so
  a relabel keeps it.
