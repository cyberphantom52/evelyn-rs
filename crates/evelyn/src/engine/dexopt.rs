//! Compiling ART's artifacts again the way the ROM's own were: the boot image
//! and an odex, by the ROM's dex2oat, run under qemu-user on the host with the
//! build's files as its root. The options are those the platform build gives
//! dex2oat, from AOSP's Soong at `android-17.0.0_r1`:
//!
//! - `build/soong/dexpreopt/dexpreopt.go` (`dexpreoptCommand`) for an odex, and
//!   `build/soong/java/dexpreopt_bootjars.go` (`buildBootImageVariant`) with
//!   `build/soong/java/dexpreopt_config.go` (`genBootImageConfigRaw`) for the
//!   boot image.
//! - Given to every compilation: `--avoid-storing-invocation`,
//!   `--generate-debug-info --strip --oat-symbols=…` (the installed file is the
//!   stripped one), `--generate-build-id`, `--force-determinism`,
//!   `--no-inline-from=core-oj.jar`. Soong also gives
//!   `--abort-on-hard-verifier-error`, which changes nothing dex2oat writes, only
//!   whether a class that fails verification stops it; a ROM's build does not
//!   always: the vdex OxygenOS ships for its Settings records such classes, and
//!   is byte for byte what dex2oat writes without it. It is left out.
//! - Product choices the files record, read from the ROM's: the instruction set
//!   features (the oat header's bitmap; `--instruction-set-variant` only picks
//!   features, so `generic` with the explicit list stands for any), the GC
//!   (`concurrent-copying`: `-Xgc:CMC` from `PRODUCT_ENABLE_UFFD_GC`), the
//!   assumed SDK version (`assume-value-sdk-int`), profiling code
//!   (`enable-profile-code`), the compiler filter, the compilation reason, mini
//!   debug info for an odex (its `.gnu_debugdata`), an app image (an `.art`
//!   beside the odex), and the primary boot image's address (`--base`, its
//!   header's `image_begin`).
//! - Product choices the files do not record, taken as the platform's defaults:
//!   `--resolve-startup-const-strings=true` for an app image (unless
//!   `PRODUCT_DEX_PREOPT_RESOLVE_STARTUP_STRINGS` is off), no extra
//!   `PRODUCT_DEX_PREOPT_DEFAULT_FLAGS`/`PRODUCT_DEX_PREOPT_BOOT_FLAGS`, and
//!   `--preloaded-classes` given to the primary boot image (unless
//!   `RELEASE_ART_NOPRELOAD_CLASSES_IN_PROFILE`). `-Xms`/`-Xmx` are left out:
//!   they bound dex2oat's heap, not what it writes.
//! - As the platform build, which runs dex2oat on the build host, does, each
//!   file is given by where it is on the host and by its location on the device
//!   apart: `--dex-file` and `--dex-location`, `-Xbootclasspath` and
//!   `-Xbootclasspath-locations`, `--class-loader-context` and
//!   `--stored-class-loader-context`, `--oat-file` and `--oat-location`; what
//!   dex2oat records is the device's. So dex2oat reads the build's files and
//!   never the host's own `/system`, whatever the host.
//! - Constants of the boot image: the primary image covers the platform's boot
//!   jars (ART's and the framework's) as one image per jar, compiled with
//!   `/system/etc/boot-image.prof` and every APEX's `etc/boot-image.prof`,
//!   `--preloaded-classes` and `--dirty-image-objects`; the extension covers the
//!   APEXes' boot jars as one image (`--single-image`), compiled against the
//!   primary (`--boot-image`) with the APEXes' profiles and
//!   `--dirty-image-objects`; both `--image-format=lz4hc`, named `boot`, which
//!   dex2oat makes `boot-<first jar>` for an extension and for each further jar
//!   of a multi-image primary.
//!
//! Compiled this way from the ROM's own jars, the boot image and odex files have
//! the ROM's layout, vdex files and key-value stores byte for byte; the code of a
//! few JNI stubs differs, as the ROM's host dex2oat was built apart from the ART
//! module whose dex2oat runs here.

use crate::engine::{Error, IoAt, device::Device};
use android::art::{self, ImageHeader, Oat};
use android::elf::Elf;
use android::partition::PartitionPath;
use std::{
    collections::BTreeSet,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

/// dex2oat and profman, in the build's APEXes.
const DEX2OAT: &str = "/apex/com.android.art/bin/dex2oat64";
const PROFMAN: &str = "/apex/com.android.art/bin/profman";
/// The ROM's linker, which runs dex2oat and profman given as its first argument.
const LINKER: &str = "/apex/com.android.runtime/bin/linker64";
/// Where dex2oat's libraries are found: the ART APEX's own, bionic, and those of
/// the APEXes and the system its linker namespace links to on the device.
const LIBRARY_PATH: [&str; 5] = [
    "/apex/com.android.art/lib64",
    "/apex/com.android.runtime/lib64/bionic",
    "/apex/com.android.i18n/lib64",
    "/apex/com.android.os.statsd/lib64",
    "/system/lib64",
];
/// A library ART opens itself rather than names as needed.
const OPENED: [&str; 1] = ["libartpalette-system.so"];
/// The directories a boot image may be in.
const BOOT_IMAGE_DIRS: [&str; 2] = ["/system/framework", "/apex/com.android.art/javalib"];
/// The boot image's inputs the platform build installs.
const BOOT_PROFILE: &str = "/system/etc/boot-image.prof";
const PRELOADED_CLASSES: &str = "/system/etc/preloaded-classes";
const DIRTY_IMAGE_OBJECTS: &str = "/system/etc/dirty-image-objects";

/// An odex to compile again.
pub(crate) struct Job {
    /// Its artifacts: where the odex is, without `.odex`.
    pub artifacts: PartitionPath,
    pub isa: String,
    /// The DEX file location it is compiled from.
    pub dex: String,
    /// The package as the ROM has it, when the package itself changed: its
    /// profile is listed against it.
    pub original: Option<Vec<u8>>,
}

impl Job {
    pub fn artifact(&self, extension: &str) -> PartitionPath {
        PartitionPath {
            partition: self.artifacts.partition.clone(),
            path: crate::engine::device::relative(format!("{}.{extension}", self.artifacts.path)),
        }
    }
}

/// One image of the boot image to compile again: the primary image (the first)
/// or an extension of the images before it.
pub(crate) struct BootJob {
    pub isa: String,
    /// Its index among the boot image's images.
    pub index: usize,
    /// The boot class path jars of the images before it, then its own.
    pub before: Vec<String>,
    pub jars: Vec<String>,
    /// The checksums of the images before it, as an oat file records them.
    pub before_checksums: String,
    /// The ROM's content of each of its jars that changed: the primary image's
    /// profile is listed against them.
    pub originals: Vec<(String, Vec<u8>)>,
}

impl BootJob {
    /// Its name: `boot` for the primary image, `boot-<first jar>` for an extension.
    pub fn name(&self) -> String {
        match self.before.is_empty() {
            true => "boot".to_owned(),
            false => format!("boot-{}", file_stem(&self.jars[0])),
        }
    }
}

/// The ROM's ART tools, runnable on the host: a directory holding them and the
/// build's files they read, at their device paths below it, and qemu-user to
/// run them on a host that is not arm64 Linux or Android.
pub(crate) struct Art {
    emulator: Option<PathBuf>,
    root: PathBuf,
    out: PathBuf,
}

impl Art {
    /// dex2oat and profman, the APEXes on their library path, their linker, and
    /// the system libraries they need, in a new directory in `work`, run by
    /// `emulator` if the host needs one.
    pub fn new(
        device: &mut Device<'_>,
        emulator: Option<PathBuf>,
        work: &Path,
    ) -> Result<Self, Error> {
        let art = Self {
            emulator,
            root: work.join("root"),
            out: work.join("out"),
        };
        for dir in LIBRARY_PATH {
            if let Some(name) = dir.strip_prefix("/apex/").and_then(|d| d.split('/').next()) {
                for (file, data) in device.apex(name)? {
                    art.place(&format!("/apex/{name}/{file}"), &data)?;
                }
            }
        }
        let mut pending = vec![DEX2OAT.to_owned(), PROFMAN.to_owned()];
        let mut seen = BTreeSet::new();
        let mut libraries: Vec<String> = OPENED.iter().map(|name| (*name).to_owned()).collect();
        while let Some(path) = pending.pop() {
            let elf = fs::read(art.host(&path)).at(art.host(&path))?;
            libraries.extend(Elf::parse(&elf)?.needed()?);
            for library in libraries.drain(..) {
                if !seen.insert(library.clone()) {
                    continue;
                }
                for dir in LIBRARY_PATH {
                    let path = format!("{dir}/{library}");
                    if art.provide(device, &path)? {
                        pending.push(path);
                        break;
                    }
                }
            }
        }
        Ok(art)
    }

    /// Put the build's file at `path` in the root, if the build has it.
    fn provide(&self, device: &mut Device<'_>, path: &str) -> Result<bool, Error> {
        if self.host(path).exists() {
            return Ok(true);
        }
        match device.read(path)? {
            Some(data) => self.place(path, &data).map(|()| true),
            None => Ok(false),
        }
    }

    /// Write `data` at the device path `path` in the directory, runnable.
    fn place(&self, path: &str, data: &[u8]) -> Result<(), Error> {
        let host = self.host(path);
        write(&host, data)?;
        fs::set_permissions(&host, fs::Permissions::from_mode(0o755)).at(&host)
    }

    /// Where the device path `path` is on the host.
    fn host(&self, path: &str) -> PathBuf {
        self.root.join(path.trim_start_matches('/'))
    }

    /// `host` of `path`, as an argument.
    fn at(&self, path: &str) -> String {
        self.host(path).display().to_string()
    }

    /// A class loader context with its jars where they are on the host.
    fn context_at(&self, context: &str) -> String {
        regex::Regex::new(r"/[^\[\]{}:;*!#]+")
            .expect("valid")
            .replace_all(context, |found: &regex::Captures<'_>| self.at(&found[0]))
            .into_owned()
    }

    /// `program` run by the ROM's linker, under the emulator if the host needs
    /// one, with the device's environment, its directories on the host.
    fn command(&self, program: &str) -> Command {
        let mut command = match &self.emulator {
            Some(emulator) => {
                let mut command = Command::new(emulator);
                command.arg(self.host(LINKER));
                command
            }
            None => Command::new(self.host(LINKER)),
        };
        let libraries: Vec<String> = LIBRARY_PATH.iter().map(|dir| self.at(dir)).collect();
        command
            .arg(self.host(program))
            .env_clear()
            .env("LD_LIBRARY_PATH", libraries.join(":"))
            .env("ANDROID_ROOT", self.host("/system"))
            .env("ANDROID_ART_ROOT", self.host("/apex/com.android.art"))
            .env("ANDROID_I18N_ROOT", self.host("/apex/com.android.i18n"))
            .env("ANDROID_DATA", self.host("/data"));
        command
    }

    /// Run `program` with `args` to success; what it printed.
    fn run(&self, program: &'static str, args: &[String]) -> Result<Vec<u8>, Error> {
        tracing::debug!(program, ?args, "running");
        let output = self
            .command(program)
            .args(args)
            .output()
            .map_err(|source| {
                let program = self.emulator.clone().unwrap_or(self.host(LINKER));
                Error::ArtRun(program, source)
            })?;
        match output.status.success() {
            true => Ok(output.stdout),
            false => Err(Error::ArtFailed(program, output.status)),
        }
    }

    /// Compile one image of the boot image again, into the root for what is
    /// compiled after it; the files it writes over the build's, by where they go,
    /// and its first file's header.
    pub fn compile_boot(
        &self,
        job: &BootJob,
        device: &mut Device<'_>,
    ) -> Result<(Vec<(PartitionPath, PathBuf)>, ImageHeader), Error> {
        let name = job.name();
        let (dir, stock_art) = self.boot_image(device, &job.isa, &name)?;
        let stock_header = ImageHeader::read(&stock_art)?;
        let stock = Oat::read(
            &device
                .read(&format!("{dir}/{}/{name}.oat", job.isa))?
                .ok_or_else(|| Error::NoBootImage(name.clone()))?,
        )?;
        let primary = job.before.is_empty();
        let mut before = Vec::new();
        let mut start = 0;
        let counts = match primary {
            true => Vec::new(),
            false => art::boot_image_components(&job.before_checksums)?,
        };
        for count in counts {
            let name = match start {
                0 => "boot".to_owned(),
                _ => format!("boot-{}", file_stem(&job.before[start])),
            };
            let dir = self.boot_image(device, &job.isa, &name)?.0;
            before.push(self.at(&format!("{dir}/{name}.art")));
            start += count;
        }
        let classpath: Vec<&str> = job
            .before
            .iter()
            .chain(&job.jars)
            .map(String::as_str)
            .collect();
        for jar in &classpath {
            self.provide(device, jar)?;
        }

        let out = self.out.join(&job.isa).join(&name);
        fs::create_dir_all(out.join("symbols")).at(&out)?;
        let files: Vec<String> = classpath.iter().map(|jar| self.at(jar)).collect();
        let classpath = classpath.join(":");
        let mut args = common(&stock)?;
        args.extend([
            "--runtime-arg".to_owned(),
            format!("-Xbootclasspath:{}", files.join(":")),
            "--runtime-arg".to_owned(),
            format!("-Xbootclasspath-locations:{classpath}"),
            format!("--oat-symbols={}", out.join("symbols/boot.oat").display()),
            format!("--oat-file={}", out.join("boot.oat").display()),
            format!("--oat-location={dir}/{}/boot.oat", job.isa),
            format!("--image={}", out.join("boot.art").display()),
            "--image-format=lz4hc".to_owned(),
        ]);
        for jar in &job.jars {
            args.push(format!("--dex-file={}", self.at(jar)));
            args.push(format!("--dex-location={jar}"));
        }
        match primary {
            true => {
                args.push(format!("--base={:#x}", stock_header.image_begin));
                if self.provide(device, PRELOADED_CLASSES)? {
                    args.push(format!(
                        "--preloaded-classes={}",
                        self.at(PRELOADED_CLASSES)
                    ));
                }
            }
            false => {
                args.push(format!("--boot-image={}", before.join(":")));
                args.push("--single-image".to_owned());
            }
        }
        if self.provide(device, DIRTY_IMAGE_OBJECTS)? {
            args.push(format!(
                "--dirty-image-objects={}",
                self.at(DIRTY_IMAGE_OBJECTS)
            ));
        }
        let mut written = Vec::new();
        if stock.get("compiler-filter")? == "speed-profile" {
            if primary && self.provide(device, BOOT_PROFILE)? {
                let profile = match job.originals.is_empty() {
                    true => self.host(BOOT_PROFILE),
                    false => {
                        let made = self.boot_profile(job, &out)?;
                        if let Some(at) = device.locate(BOOT_PROFILE)? {
                            written.push((at, made.clone()));
                        }
                        made
                    }
                };
                args.push(format!("--profile-file={}", profile.display()));
            }
            for apex in device.apex_names()? {
                let profile = format!("/apex/{apex}/etc/boot-image.prof");
                if self.provide(device, &profile)? {
                    args.push(format!("--profile-file={}", self.at(&profile)));
                }
            }
        }
        self.run(DEX2OAT, &args)?;

        let header = ImageHeader::read(&fs::read(out.join(format!("{name}.art"))).at(&out)?)?;
        let built = Oat::read(&fs::read(out.join(format!("{name}.oat"))).at(&out)?)?;
        for (key, stock, built) in [
            (
                "component count",
                stock_header.component_count,
                header.component_count,
            ),
            ("address", stock_header.image_begin, header.image_begin),
            (
                "extended component count",
                stock_header.boot_image_component_count,
                header.boot_image_component_count,
            ),
        ] {
            if stock != built {
                return Err(Error::ArtDiffers {
                    key,
                    stock: format!("{stock:#x}"),
                    built: format!("{built:#x}"),
                });
            }
        }
        let checksums = match primary {
            true => "",
            false => job.before_checksums.as_str(),
        };
        check(&stock, &built, checksums)?;
        for entry in fs::read_dir(&out).at(&out)? {
            let entry = entry.at(&out)?;
            if !entry.file_type().at(entry.path())?.is_file() {
                continue;
            }
            let file = entry.file_name().to_string_lossy().into_owned();
            if ![".art", ".oat", ".vdex"].iter().any(|e| file.ends_with(e)) {
                continue;
            }
            let path = format!("{dir}/{}/{file}", job.isa);
            fs::copy(entry.path(), self.host(&path)).at(entry.path())?;
            if let Some(at) = device.locate(&path)? {
                written.push((at, entry.path()));
            }
        }
        Ok((written, header))
    }

    /// The primary image's profile, made again against the changed jars, as the
    /// odex profiles are (see [`Art::profile`]), as a boot image profile.
    fn boot_profile(&self, job: &BootJob, out: &Path) -> Result<PathBuf, Error> {
        let mut rom = Vec::new();
        for jar in &job.jars {
            match job.originals.iter().find(|(path, _)| path == jar) {
                Some((_, data)) => {
                    let host = out.join("rom").join(file_stem(jar)).with_extension("jar");
                    write(&host, data)?;
                    rom.push(format!("--apk={}", host.display()));
                }
                None => rom.push(format!("--apk={}", self.at(jar))),
            }
        }
        let locations: Vec<String> = job
            .jars
            .iter()
            .map(|jar| format!("--dex-location={jar}"))
            .collect();
        let mut dump = vec![
            "--dump-classes-and-methods".to_owned(),
            format!("--profile-file={}", self.at(BOOT_PROFILE)),
        ];
        dump.extend(rom);
        dump.extend(locations.iter().cloned());
        let text = out.join("boot-image.txt");
        write(&text, &self.run(PROFMAN, &dump)?)?;
        let made = out.join("boot-image.prof");
        let mut create = vec![
            "--output-profile-type=boot".to_owned(),
            format!("--create-profile-from={}", text.display()),
            format!("--reference-profile-file={}", made.display()),
        ];
        create.extend(job.jars.iter().map(|jar| format!("--apk={}", self.at(jar))));
        create.extend(locations);
        self.run(PROFMAN, &create)?;
        Ok(made)
    }

    /// Compile `job`'s odex again, against a boot image whose checksums are
    /// `checksums`; the files it writes over the build's, by where they go, the
    /// new profile among them when the package itself changed.
    pub fn compile(
        &self,
        job: &Job,
        device: &mut Device<'_>,
        checksums: &str,
    ) -> Result<Vec<(PartitionPath, PathBuf)>, Error> {
        let stock_odex = device.read_at(&job.artifact("odex"))?;
        let stock = Oat::read(&stock_odex)?;
        let debug_data = Elf::parse(&stock_odex)?
            .sections()
            .iter()
            .any(|section| section.name == ".gnu_debugdata");
        let image = device.targets[&job.artifacts.partition]
            .tree
            .entries
            .contains_key(&job.artifact("art").path);

        let classpath = stock.get("bootclasspath")?.to_owned();
        let jars: Vec<&str> = classpath.split(':').collect();
        let mut boot_image = Vec::new();
        let mut start = 0;
        for count in art::boot_image_components(checksums)? {
            let name = match start {
                0 => "boot".to_owned(),
                _ => format!("boot-{}", file_stem(jars[start])),
            };
            let dir = self.boot_image(device, &job.isa, &name)?.0;
            boot_image.push(self.at(&format!("{dir}/{name}.art")));
            start += count;
        }
        let loader = without_checksums(stock.get("classpath")?);
        for path in jars.iter().map(|jar| jar.to_string()).chain(paths(&loader)) {
            self.provide(device, &path)?;
        }
        self.provide(device, &job.dex)?;

        let name = job
            .artifacts
            .path
            .as_ref()
            .rsplit('/')
            .next()
            .unwrap_or_default();
        let output = |extension: &str| self.out.join(&job.isa).join(format!("{name}.{extension}"));
        fs::create_dir_all(self.out.join(&job.isa).join("symbols")).at(&self.out)?;
        let filter = stock.get("compiler-filter")?;
        let files: Vec<String> = jars.iter().map(|jar| self.at(jar)).collect();
        let mut args = common(&stock)?;
        args.extend([
            "--runtime-arg".to_owned(),
            format!("-Xbootclasspath:{}", files.join(":")),
            "--runtime-arg".to_owned(),
            format!("-Xbootclasspath-locations:{classpath}"),
            format!("--boot-image={}", boot_image.join(":")),
            format!("--class-loader-context={}", self.context_at(&loader)),
            format!("--stored-class-loader-context={loader}"),
            format!("--dex-file={}", self.at(&job.dex)),
            format!("--dex-location={}", job.dex),
            format!("--oat-file={}", output("odex").display()),
            format!("--output-vdex={}", output("vdex").display()),
            format!(
                "--oat-symbols={}",
                self.out
                    .join(&job.isa)
                    .join("symbols")
                    .join(format!("{name}.odex"))
                    .display()
            ),
            format!("--compilation-reason={}", stock.get("compilation-reason")?),
            match debug_data {
                true => "--generate-mini-debug-info",
                false => "--no-generate-mini-debug-info",
            }
            .to_owned(),
        ]);
        if image {
            args.push(format!("--app-image-file={}", output("art").display()));
            args.push("--image-format=lz4".to_owned());
            args.push("--resolve-startup-const-strings=true".to_owned());
        }
        let mut written = Vec::new();
        if filter.ends_with("-profile") {
            let profile = format!("{}.prof", job.dex);
            self.provide(device, &profile)?;
            match &job.original {
                None => args.push(format!("--profile-file={}", self.at(&profile))),
                Some(original) => {
                    let made = self.profile(job, &profile, original, &output("prof"))?;
                    args.push(format!("--profile-file={}", made.display()));
                    if let Some(at) = device.locate(&profile)? {
                        written.push((at, made));
                    }
                }
            }
        }
        self.run(DEX2OAT, &args)?;

        let odex = fs::read(output("odex")).at(output("odex"))?;
        check(&stock, &Oat::read(&odex)?, checksums)?;
        for extension in ["odex", "vdex"].into_iter().chain(image.then_some("art")) {
            written.push((job.artifact(extension), output(extension)));
        }
        Ok(written)
    }

    /// The profile of a changed package. A profile names each DEX file by its
    /// checksum, so the ROM's no longer reaches the changed ones. Like the
    /// platform build, which makes it from a listing of classes and methods, it
    /// is listed against the package as the ROM has it and made again against
    /// the changed one.
    fn profile(
        &self,
        job: &Job,
        profile: &str,
        original: &[u8],
        made: &Path,
    ) -> Result<PathBuf, Error> {
        let rom = made.with_extension("rom.jar");
        write(&rom, original)?;
        let listing = self.run(
            PROFMAN,
            &[
                "--dump-classes-and-methods".to_owned(),
                format!("--profile-file={}", self.at(profile)),
                format!("--apk={}", rom.display()),
                format!("--dex-location={}", job.dex),
            ],
        )?;
        let text = made.with_extension("txt");
        write(&text, &listing)?;
        self.run(
            PROFMAN,
            &[
                format!("--create-profile-from={}", text.display()),
                format!("--apk={}", self.at(&job.dex)),
                format!("--dex-location={}", job.dex),
                format!("--reference-profile-file={}", made.display()),
            ],
        )?;
        Ok(made.to_path_buf())
    }

    /// The directory of the boot image component `name` for `isa`, with its
    /// files, and those of the images of the further jars it covers, in the root;
    /// and its `.art` file as the root has it.
    fn boot_image(
        &self,
        device: &mut Device<'_>,
        isa: &str,
        name: &str,
    ) -> Result<(String, Vec<u8>), Error> {
        for dir in BOOT_IMAGE_DIRS {
            let art = format!("{dir}/{isa}/{name}.art");
            if !self.provide(device, &art)? {
                continue;
            }
            for file in device.list(&format!("{dir}/{isa}"))? {
                if file.starts_with("boot") && !file.ends_with(".fsv_meta") {
                    self.provide(device, &format!("{dir}/{isa}/{file}"))?;
                }
            }
            let data = fs::read(self.host(&art)).at(self.host(&art))?;
            return Ok((dir.to_owned(), data));
        }
        Err(Error::NoBootImage(name.to_owned()))
    }
}

/// What the platform build gives every compilation, with the product's choices
/// that `stock`, the ROM's oat file, records.
fn common(stock: &Oat) -> Result<Vec<String>, Error> {
    let mut args = vec![
        "--avoid-storing-invocation".to_owned(),
        "--generate-debug-info".to_owned(),
        "--strip".to_owned(),
        "--generate-build-id".to_owned(),
        "--force-determinism".to_owned(),
        "--no-inline-from=core-oj.jar".to_owned(),
        format!("--instruction-set={}", stock.isa()?),
        "--instruction-set-variant=generic".to_owned(),
        format!("--instruction-set-features={}", stock.feature_list()?),
        format!("--compiler-filter={}", stock.get("compiler-filter")?),
        "--runtime-arg".to_owned(),
        match stock.get("concurrent-copying")? {
            "true" => "-Xgc:CC",
            _ => "-Xgc:CMC",
        }
        .to_owned(),
        format!(
            "-j{}",
            std::thread::available_parallelism().map_or(1, |n| n.get())
        ),
    ];
    if let Some(sdk) = stock.store.get("assume-value-sdk-int") {
        args.push(format!(
            "--assume-value=Landroid/os/Build$VERSION;->SDK_INT:{sdk}"
        ));
    }
    if stock.store.get("enable-profile-code").map(String::as_str) == Some("true") {
        args.push("--allow-profile-code".to_owned());
    }
    Ok(args)
}

/// What the new oat file must record as the ROM's did for the device to use it:
/// the boot class path and how it was compiled, the instruction set features,
/// the class loader context (whose checksums are of the jars as the build has
/// them), and `checksums`, the boot image it was compiled against as the build
/// has it ("" for the primary image).
fn check(stock: &Oat, built: &Oat, checksums: &str) -> Result<(), Error> {
    let differs = |key: &'static str, stock: String, built: String| match stock == built {
        true => Ok(()),
        false => Err(Error::ArtDiffers { key, stock, built }),
    };
    for key in [
        "bootclasspath",
        "compiler-filter",
        "compilation-reason",
        "concurrent-copying",
    ] {
        let get = |oat: &Oat| oat.store.get(key).cloned().unwrap_or_default();
        differs(key, get(stock), get(built))?;
    }
    let get = |oat: &Oat| {
        oat.store
            .get("bootclasspath-checksums")
            .cloned()
            .unwrap_or_default()
    };
    differs("bootclasspath-checksums", checksums.to_owned(), get(built))?;
    let classpath = |oat: &Oat| without_checksums(oat.store.get("classpath").map_or("", |s| s));
    differs("classpath", classpath(stock), classpath(built))?;
    differs(
        "instruction set features",
        format!("{:#x}", stock.features),
        format!("{:#x}", built.features),
    )
}

/// A class loader context with the jars' checksums (`*<checksum>`) left out.
fn without_checksums(context: &str) -> String {
    regex::Regex::new(r"\*[0-9]+")
        .expect("valid")
        .replace_all(context, "")
        .into_owned()
}

/// The jars a class loader context names.
pub(crate) fn paths(context: &str) -> Vec<String> {
    regex::Regex::new(r"/[^\[\]{}:;*!]+")
        .expect("valid")
        .find_iter(context)
        .map(|found| found.as_str().to_owned())
        .collect()
}

/// A file name without its extension.
pub(crate) fn file_stem(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').map_or(name, |(stem, _)| stem)
}

fn write(path: &Path, data: &[u8]) -> Result<(), Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).at(parent)?;
    }
    fs::write(path, data).at(path)
}
