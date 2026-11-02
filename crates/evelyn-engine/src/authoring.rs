//! Prototype a file edit: prepare an editable copy, edit it, capture a recipe.

pub use patch::capture::CaptureKind;

use crate::Error;
use crate::Plan;
use android_firmware::{DevicePath, Mounts, Name, RelativePath, Sha256Digest};
use android_fs::Content;
use recipe::{Recipe, Step};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};
use toolchain::Tools;

/// Written to `prepare.json` so capture knows what the edit applies to.
#[derive(Debug, Serialize, Deserialize)]
struct Preparation {
    path: DevicePath,
    before: Sha256Digest,
}

pub struct PrepareRequest<'a> {
    pub plan: &'a Plan,
    pub tools: &'a Tools,
    /// The build workspace whose last composed targets are read.
    pub work_dir: &'a Path,
    /// The file, as the device sees it.
    pub path: DevicePath,
    /// Where the editable copy is made.
    pub into: &'a Path,
}

#[derive(Debug, Serialize)]
pub struct PreparedFile {
    pub workspace: PathBuf,
    pub edited: PathBuf,
    pub before: Sha256Digest,
}

/// Copy one file, as the last build composed it, into `into/<device path>/` as
/// `original` and `edited`.
pub fn prepare_file(request: PrepareRequest<'_>) -> Result<PreparedFile, Error> {
    let targets = request.work_dir.join("targets");
    let mut mounts = Vec::new();
    for partition in &request.plan.partitions {
        let mount = match &partition.mount_point {
            Some(mount) => mount.clone(),
            None => {
                let tree = android_fs::read_tree(&targets.join(partition.name.as_ref())).map_err(
                    |_| Error::Authoring("no composed target; run `evelyn build` first"),
                )?;
                crate::build::mount_point(partition, &tree)?
            }
        };
        mounts.push((mount, partition.name.clone()));
    }
    let target = match Mounts::new(mounts).resolve(&request.path).as_slice() {
        [target] => target.clone(),
        _ => return Err(Error::Authoring("the path is not in exactly one partition")),
    };
    let root = targets.join(target.partition.as_ref());
    let tree = android_fs::read_tree(&root)
        .map_err(|_| Error::Authoring("no composed target; run `evelyn build` first"))?;
    let file = tree
        .file(&target.path)
        .map_err(|_| Error::Authoring("only regular files can be prepared"))?;
    let mut images = patch::Images::new();
    if let Content::Image { source, .. } = file.content {
        let artifact = &request
            .plan
            .sources
            .iter()
            .find(|planned| &planned.source == source)
            .ok_or(Error::Authoring(
                "file comes from a source the profile no longer uses",
            ))?
            .artifact;
        let image = crate::build::open_image(source, artifact)?;
        images.insert(source.clone(), image);
    }
    let workspace = request
        .into
        .join(request.path.as_ref().trim_start_matches('/'));
    if workspace.exists() {
        fs::remove_dir_all(&workspace)?;
    }
    fs::create_dir_all(&workspace)?;
    std::io::copy(
        &mut android_fs::Store::new(&images, &root).open(file.content, file.size)?,
        &mut fs::File::create(workspace.join("original"))?,
    )?;
    fs::copy(workspace.join("original"), workspace.join("edited"))?;
    let record = Preparation {
        path: request.path,
        before: file.sha256.clone(),
    };
    fs::write(
        workspace.join("prepare.json"),
        serde_json::to_vec_pretty(&record)?,
    )?;
    Ok(PreparedFile {
        edited: workspace.join("edited"),
        workspace,
        before: record.before,
    })
}

pub struct CaptureRequest<'a> {
    pub workspace: &'a Path,
    pub output: &'a Path,
    pub name: Name,
    pub kind: CaptureKind,
}

#[derive(Debug, Serialize)]
pub struct CapturedRecipe {
    pub recipe: PathBuf,
    pub asset: Option<PathBuf>,
    pub after: Sha256Digest,
}

/// Turn the edits made to `edited` into a new patch directory at `output`.
pub fn capture_file(request: CaptureRequest<'_>) -> Result<CapturedRecipe, Error> {
    let record: Preparation =
        serde_json::from_slice(&fs::read(request.workspace.join("prepare.json"))?)?;
    let original = fs::read(request.workspace.join("original"))?;
    if Sha256Digest::bytes(&original) != record.before {
        return Err(Error::Authoring(
            "original was modified; edit `edited` instead",
        ));
    }
    let edited = fs::read(request.workspace.join("edited"))?;
    let asset = RelativePath::try_from(
        match request.kind {
            CaptureKind::Replace => "replacement.bin",
            _ => "change.patch",
        }
        .to_owned(),
    )?;
    let captured = patch::capture::capture(
        &original,
        &edited,
        record.path.clone(),
        asset.clone(),
        request.kind,
    )?;
    let recipe = Recipe {
        name: request.name.clone(),
        steps: vec![Step {
            id: request.name,
            after: Vec::new(),
            bindings: Default::default(),
            when: None,
            operations: captured.operations,
        }],
    };
    if let Some(parent) = request.output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir(request.output)?;
    let asset = match captured.asset {
        Some(bytes) => {
            let path = request.output.join(asset.as_ref());
            fs::write(&path, bytes)?;
            Some(path)
        }
        None => None,
    };
    let path = request.output.join(crate::profile::PATCH);
    recipe.write(fs::File::create_new(&path)?)?;
    Ok(CapturedRecipe {
        recipe: path,
        asset,
        after: captured.after,
    })
}
