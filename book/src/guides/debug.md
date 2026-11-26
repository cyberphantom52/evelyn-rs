# Debug builds

A debug build records boot logs and turns adb on from boot:

```console
$ evelyn build --debug
```

After every other recipe, Evelyn applies one more, which is built in. It does
two things.

## Boot logs

It installs an init service at `/vendor/etc/init/logcat.rc`. The service writes
logcat and the kernel log to `/data/local/tmp/logs` until boot completes. If
the device reaches `/data` but never finishes booting, the logs are there for
the next boot, or for recovery.

## adb from boot

It sets these properties in `/system/build.prop`:

| Property                  | Value     |
|---------------------------|-----------|
| `ro.adb.secure`           | `0`       |
| `ro.secure`               | `0`       |
| `ro.debuggable`           | `1`       |
| `persist.sys.usb.config`  | `mtp,adb` |

adb is then on from early boot and needs no authorisation. You can run
`adb logcat` while the device is still booting.

> **Warning:** A debug build has no adb authorisation and is debuggable. Do not
> ship it, and do not use it as your daily build.

## Permissive SELinux

A permissive build starts SELinux permissive, so a denial is logged rather
than enforced:

```console
$ evelyn build --selinux-permissive
```

After every other recipe, Evelyn applies one more, which is built in. It sets
`androidboot.selinux=permissive` in the bootconfig of the stock `vendor_boot`,
which must be a v4 image, and builds that image again.

Android's init honours this only when it was built to allow a permissive
SELinux, as eng and userdebug builds are. A user build's init ignores it and
starts SELinux enforcing. Run `adb shell getenforce` to see which it did.

> **Warning:** A permissive device does not enforce its SELinux policy. Use it
> to find denials, then fix the policy and build without it.
