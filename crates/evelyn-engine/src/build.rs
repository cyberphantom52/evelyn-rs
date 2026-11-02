//! The build pipeline: scan sources, compose targets, apply recipes, build images.
//!
//! Source images are never extracted. Each one is scanned into a tree whose files
//! refer back to the image, and stays open for the whole build: untouched files
//! stream straight from it into the new image, and only edited files are written.
//!
//! Work directory layout (reused across builds):
//!   sources/<source>.<partition>/       scanned trees, reused while the image hash matches
//!   targets/<partition>/                composed trees and the blobs edits wrote
//!   packages/                           decoded APK/JAR workspaces
//!   images/<partition>.img              built filesystem images

use crate::{Error, Output, Plan};
use android_firmware::{Mounts, Name, RelativePath, SourceRef};
use android_fs::Image;
use android_fs::lp::SuperImage;
use android_fs::{Artifact, PartitionTree};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use toolchain::Tools;

pub struct BuildRequest<'a> {
    pub plan: &'a Plan,
    pub tools: &'a Tools,
    pub work_dir: &'a Path,
    pub output_dir: &'a Path,
    /// Compose and apply every recipe, but build and write no images.
    pub dry_run: bool,
}

#[derive(Debug, Serialize)]
pub struct Built {
    /// What was written; nothing for a dry run.
    pub outputs: Vec<PathBuf>,
    /// Steps whose bindings or condition did not apply here.
    pub skipped: Vec<patch::Skipped>,
    /// `SetMetadata` patterns that set what was already there.
    pub unchanged: Vec<patch::Unchanged>,
    /// ROM partitions the port does not take.
    pub ignored: Vec<Name>,
}

pub fn build(request: &BuildRequest<'_>) -> Result<Built, Error> {
    let plan = request.plan;
    let mut images = patch::Images::new();
    let mut sources = patch::Sources::new();
    for source in &plan.sources {
        let (image, tree) = open(request, &source.source, &source.artifact)?;
        images.insert(source.source.clone(), image);
        sources.insert(source.source.clone(), tree);
    }
    let mounts = mounts(plan, &sources)?;
    let (targets, report) = compose(request, &sources, &images, &mounts)?;
    for skip in &report.skipped {
        tracing::warn!(recipe = %skip.recipe, step = %skip.step, reason = %skip.reason, "skipped step");
    }
    for unchanged in &report.unchanged {
        tracing::warn!(step = %unchanged.step, pattern = %unchanged.pattern, "SetMetadata sets what is already there");
    }
    crate::fstab::check(&targets, &images)?;
    if request.dry_run {
        return Ok(Built {
            outputs: Vec::new(),
            skipped: report.skipped,
            unchanged: report.unchanged,
            ignored: plan.ignored.clone(),
        });
    }
    let output = request.work_dir.join("images");
    reset(&output)?;
    // Builds `partition` into `to`: a region of `super`, or its own image file.
    let build = |partition: &crate::PlannedPartition, to: To| {
        let (root, tree) = &targets[&partition.name];
        tracing::info!(partition = %partition.name, filesystem = ?tree.filesystem, "building image");
        let store = android_fs::Store::new(&images, root);
        let mount = mount_point(partition, &sources[&partition.base])?;
        let (name, filesystem) = (&partition.name, tree.filesystem);
        Ok::<_, Error>(match to {
            To::Region(region) => {
                android_fs::build::image(tree, &store, region, filesystem, name, &mount)?
            }
            To::File(image) => {
                android_fs::build::image_file(tree, &store, image, filesystem, name, &mount)?
            }
        })
    };
    let built = match plan.output {
        Output::Partitions => {
            let mut images = Vec::new();
            for partition in &plan.partitions {
                let image = output.join(format!("{}.img", partition.name));
                build(partition, To::File(&image))?;
                images.push((partition.name.clone(), image));
            }
            Written::Partitions(images)
        }
        // Every partition is built straight into its place in `super`.
        Output::Super | Output::Zip => {
            let layout = plan
                .profile
                .device
                .super_layout
                .clone()
                .expect("checked by Plan::load");
            let path = output.join("super.img");
            let mut image = SuperImage::create(&path, layout)?;
            for partition in &plan.partitions {
                let size = build(partition, To::Region(image.region()?))?;
                image.add(partition.name.clone(), size)?;
            }
            image.finish()?;
            Written::Super(path)
        }
    };
    let outputs = crate::output::publish(request, built)?;
    Ok(Built {
        outputs,
        skipped: report.skipped,
        unchanged: report.unchanged,
        ignored: plan.ignored.clone(),
    })
}

fn reset(path: &Path) -> Result<(), Error> {
    if path.exists() {
        fs::remove_dir_all(path)?;
    }
    fs::create_dir_all(path)?;
    Ok(())
}

/// Open a source image for reading; sparse images are read as the raw image they hold.
pub(crate) fn open_image(source: &SourceRef, artifact: &Artifact) -> Result<Image, Error> {
    Image::detect(&artifact.path).map_err(|_| Error::SourceFilesystem(source.clone()))
}

/// Open a source image and its scanned tree, scanning only when the image changed.
fn open(
    request: &BuildRequest<'_>,
    source: &SourceRef,
    artifact: &Artifact,
) -> Result<(Image, PartitionTree), Error> {
    let image = open_image(source, artifact)?;
    let cache = request.work_dir.join("sources").join(source.to_string());
    if let Ok(tree) = android_fs::read_tree(&cache)
        && tree.source_sha256 == artifact.sha256
        && tree.partition == source.partition
    {
        return Ok((image, tree));
    }
    tracing::info!(%source, "scanning source image");
    let tree = android_fs::scan(&image, source, artifact.sha256.clone())?;
    fs::create_dir_all(&cache)?;
    android_fs::write_tree(&cache, &tree)?;
    Ok((image, tree))
}

pub(crate) type Targets = BTreeMap<Name, (PathBuf, PartitionTree)>;

/// Where a partition is built.
enum To<'a> {
    Region(android_fs::Region),
    File(&'a Path),
}

/// What a build wrote: each partition's image in `super` order, or one `super` image
/// holding them all.
pub(crate) enum Written {
    Partitions(Vec<(Name, PathBuf)>),
    Super(PathBuf),
}

/// Where `partition` is mounted: the plan's, or for `system`, as `tree` is laid out.
pub(crate) fn mount_point(
    partition: &crate::PlannedPartition,
    tree: &PartitionTree,
) -> Result<String, Error> {
    if let Some(mount) = &partition.mount_point {
        return Ok(mount.clone());
    }
    let has = |path: &str| {
        RelativePath::try_from(path.to_owned()).is_ok_and(|path| tree.entries.contains_key(&path))
    };
    android_firmware::system_mount_point(has)
        .map(str::to_owned)
        .ok_or(Error::SystemLayout)
}

/// Every partition's mount point, for resolving device paths.
pub(crate) fn mounts(plan: &Plan, sources: &patch::Sources) -> Result<Mounts, Error> {
    let mut mounts = Vec::new();
    for partition in &plan.partitions {
        mounts.push((
            mount_point(partition, &sources[&partition.base])?,
            partition.name.clone(),
        ));
    }
    Ok(Mounts::new(mounts))
}

/// Start each partition from its base tree and apply every recipe in plan order.
fn compose(
    request: &BuildRequest<'_>,
    sources: &patch::Sources,
    images: &patch::Images,
    mounts: &Mounts,
) -> Result<(Targets, patch::Report), Error> {
    let plan = request.plan;
    let root = request.work_dir.join("targets");
    reset(&root)?;
    let mut targets = BTreeMap::new();
    for partition in &plan.partitions {
        let base = sources[&partition.base].clone();
        let filesystem = partition.filesystem.unwrap_or(base.filesystem);
        let target = patch::Target::create(
            &root.join(partition.name.as_ref()),
            base,
            partition.name.clone(),
            filesystem,
        )?;
        targets.insert(partition.name.clone(), target);
    }
    let packages = request.work_dir.join("packages");
    reset(&packages)?;
    let origins = plan
        .partitions
        .iter()
        .map(|partition| (partition.name.clone(), partition.base.source))
        .collect();
    let mut composition = patch::Composition::new(targets, origins);
    for planned in &plan.recipes {
        tracing::info!(recipe = %planned.recipe.name, "applying recipe");
        let context = patch::Context {
            sources,
            images,
            tools: request.tools,
            assets: planned
                .path
                .as_deref()
                .map(|path| path.parent().unwrap_or(Path::new("."))),
            packages: &packages,
            mounts,
        };
        composition.apply(&planned.tree, &context)?;
    }
    let (trees, report) = composition.finish(mounts, images)?;
    let targets = trees
        .into_iter()
        .map(|(name, tree)| {
            let path = root.join(name.as_ref());
            (name, (path, tree))
        })
        .collect();
    Ok((targets, report))
}
