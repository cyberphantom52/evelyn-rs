//! Compiling an odex again the way the ROM's own was: by the ROM's dex2oat, run
//! under qemu-user on the host with the build's files as its root, given what the
//! odex's oat header records (compiler filter, class loader context, boot class
//! path and image, instruction set features, assumed SDK version) and the options
//! the platform build compiles with. The new odex must record the boot image and
//! class loader context the old one did, or the device would reject it too.

use crate::engine::{Error, IoAt, device::Device};
use android::art::{self, Oat};
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
/// The linker dex2oat names as its interpreter, and where the ROM has its file.
const LINKER: (&str, &str) = (
    "/system/bin/linker64",
    "/apex/com.android.runtime/bin/linker64",
);
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

/// The ROM's ART tools, runnable on the host: a root holding them and the
/// build's files they read, and qemu-user to run them in it.
pub(crate) struct Art {
    qemu: PathBuf,
    root: PathBuf,
    out: PathBuf,
}

impl Art {
    /// dex2oat and profman, the APEXes on their library path, their linker, and
    /// the system libraries they need, in a new root in `work`, run by `qemu`.
    pub fn new(device: &mut Device<'_>, qemu: PathBuf, work: &Path) -> Result<Self, Error> {
        let art = Self {
            qemu,
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
        let linker = fs::read(art.host(LINKER.1)).at(art.host(LINKER.1))?;
        art.place(LINKER.0, &linker)?;
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

    /// Write `data` at the device path `path` in the root, runnable, as qemu-user
    /// runs only what it may execute.
    fn place(&self, path: &str, data: &[u8]) -> Result<(), Error> {
        let host = self.host(path);
        write(&host, data)?;
        fs::set_permissions(&host, fs::Permissions::from_mode(0o755)).at(&host)
    }

    fn host(&self, path: &str) -> PathBuf {
        self.root.join(path.trim_start_matches('/'))
    }

    /// `program` under qemu-user, with the root as `/` for the files it opens that
    /// the root has, and the environment it has on the device.
    fn command(&self, program: &str) -> Command {
        let mut command = Command::new(&self.qemu);
        command
            .arg("-L")
            .arg(&self.root)
            .arg(self.host(program))
            .env_clear()
            .env("LD_LIBRARY_PATH", LIBRARY_PATH.join(":"))
            .env("ANDROID_ROOT", "/system")
            .env("ANDROID_ART_ROOT", "/apex/com.android.art")
            .env("ANDROID_I18N_ROOT", "/apex/com.android.i18n")
            .env("ANDROID_DATA", "/data");
        command
    }

    /// Run `program` with `args` to success; what it printed.
    fn run(&self, program: &'static str, args: &[String]) -> Result<Vec<u8>, Error> {
        let output = self
            .command(program)
            .args(args)
            .output()
            .map_err(|source| Error::ArtRun(self.qemu.clone(), source))?;
        match output.status.success() {
            true => Ok(output.stdout),
            false => Err(Error::ArtFailed(program, output.status)),
        }
    }

    /// Compile `job`'s odex again; the files it writes over the build's, by where
    /// they go, the new profile among them when the package itself changed.
    pub fn compile(
        &self,
        job: &Job,
        device: &mut Device<'_>,
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
        for count in art::boot_image_components(stock.get("bootclasspath-checksums")?)? {
            let name = match start {
                0 => "boot".to_owned(),
                _ => format!("boot-{}", file_stem(jars[start])),
            };
            boot_image.push(self.boot_image(device, &job.isa, &name)?);
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
        let output = |extension: &str| self.out.join(format!("{name}.{extension}"));
        fs::create_dir_all(&self.out).at(&self.out)?;
        let filter = stock.get("compiler-filter")?;
        let mut args = vec![
            "--runtime-arg".to_owned(),
            format!("-Xbootclasspath:{classpath}"),
            "--runtime-arg".to_owned(),
            format!("-Xbootclasspath-locations:{classpath}"),
            "--runtime-arg".to_owned(),
            match stock.get("concurrent-copying")? {
                "true" => "-Xgc:CC",
                _ => "-Xgc:CMC",
            }
            .to_owned(),
            format!("--boot-image={}", boot_image.join(":")),
            format!("--class-loader-context={loader}"),
            format!("--dex-file={}", job.dex),
            format!("--dex-location={}", job.dex),
            format!("--oat-file={}", output("odex").display()),
            format!("--output-vdex={}", output("vdex").display()),
            format!("--instruction-set={}", stock.isa()?),
            "--instruction-set-variant=generic".to_owned(),
            format!("--instruction-set-features={}", stock.feature_list()?),
            format!("--compiler-filter={filter}"),
            format!("--compilation-reason={}", stock.get("compilation-reason")?),
            // What the platform build compiles every package with.
            "--no-inline-from=core-oj.jar".to_owned(),
            "--avoid-storing-invocation".to_owned(),
            "--force-determinism".to_owned(),
            "--abort-on-hard-verifier-error".to_owned(),
            "--generate-build-id".to_owned(),
            format!(
                "-j{}",
                std::thread::available_parallelism().map_or(1, |n| n.get())
            ),
        ];
        if image {
            args.push(format!("--app-image-file={}", output("art").display()));
            args.push("--image-format=lz4".to_owned());
            args.push("--resolve-startup-const-strings=true".to_owned());
        }
        if let Some(sdk) = stock.store.get("assume-value-sdk-int") {
            args.push(format!(
                "--assume-value=Landroid/os/Build$VERSION;->SDK_INT:{sdk}"
            ));
        }
        // Full DWARF, which some ROMs ship, is left out: the device never maps it,
        // and the mini debug info still names the methods in stack traces.
        if debug_data {
            args.push("--generate-mini-debug-info".to_owned());
        }
        let mut written = Vec::new();
        if filter.ends_with("-profile") {
            let profile = format!("{}.prof", job.dex);
            self.provide(device, &profile)?;
            match &job.original {
                None => args.push(format!("--profile-file={profile}")),
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
        check(&stock, &Oat::read(&odex)?)?;
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
                format!("--profile-file={profile}"),
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
                format!("--apk={}", job.dex),
                format!("--dex-location={}", job.dex),
                format!("--reference-profile-file={}", made.display()),
            ],
        )?;
        Ok(made.to_path_buf())
    }

    /// The boot image component `name` for `isa`, as dex2oat names its location,
    /// with its files, and those of the images of the further jars it covers, in
    /// the root.
    fn boot_image(&self, device: &mut Device<'_>, isa: &str, name: &str) -> Result<String, Error> {
        for dir in BOOT_IMAGE_DIRS {
            if !self.provide(device, &format!("{dir}/{isa}/{name}.art"))? {
                continue;
            }
            for file in device.list(&format!("{dir}/{isa}"))? {
                if file.starts_with("boot") && !file.ends_with(".fsv_meta") {
                    self.provide(device, &format!("{dir}/{isa}/{file}"))?;
                }
            }
            return Ok(format!("{dir}/{name}.art"));
        }
        Err(Error::NoBootImage(name.to_owned()))
    }
}

/// What the new odex must record as the ROM's did for the device to use it: the
/// boot image and boot class path it was compiled against, the class loader
/// context (whose checksums are of the jars as the build has them), how it was
/// compiled, and the instruction set features.
fn check(stock: &Oat, built: &Oat) -> Result<(), Error> {
    let differs = |key: &'static str, stock: String, built: String| match stock == built {
        true => Ok(()),
        false => Err(Error::ArtDiffers { key, stock, built }),
    };
    for key in [
        "bootclasspath",
        "bootclasspath-checksums",
        "compiler-filter",
        "compilation-reason",
        "concurrent-copying",
    ] {
        let get = |oat: &Oat| oat.store.get(key).cloned().unwrap_or_default();
        differs(key, get(stock), get(built))?;
    }
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
fn file_stem(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').map_or(name, |(stem, _)| stem)
}

fn write(path: &Path, data: &[u8]) -> Result<(), Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).at(parent)?;
    }
    fs::write(path, data).at(path)
}
