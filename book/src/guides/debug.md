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
