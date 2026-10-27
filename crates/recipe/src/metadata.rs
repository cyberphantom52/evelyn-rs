use crate::Error;
use model::AndroidMetadata;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetadataSpec {
    pub uid: u32,
    pub gid: u32,
    pub mode: u16,
    pub selinux: String,
    #[serde(default)]
    pub xattrs: BTreeMap<String, Vec<u8>>,
}

impl MetadataSpec {
    /// Checks only the `user:role:type:level` shape; the policy itself is the device's concern.
    pub fn resolve(&self) -> Result<AndroidMetadata, Error> {
        let label = &self.selinux;
        if label.splitn(4, ':').filter(|part| !part.is_empty()).count() != 4
            || label.contains(|c: char| c == '\0' || c.is_whitespace())
        {
            return Err(Error::Invalid(format!("invalid SELinux label: {label:?}")));
        }
        let mut xattrs = self.xattrs.clone();
        let mut value = label.as_bytes().to_vec();
        value.push(0);
        xattrs.insert("security.selinux".into(), value);
        let metadata = AndroidMetadata {
            uid: self.uid,
            gid: self.gid,
            mode: self.mode,
            xattrs,
        };
        metadata.validate()?;
        Ok(metadata)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum MetadataPolicy {
    #[default]
    Preserve,
    Explicit(MetadataSpec),
}

impl MetadataPolicy {
    pub fn resolve(&self, existing: Option<&AndroidMetadata>) -> Result<AndroidMetadata, Error> {
        match self {
            Self::Preserve => existing.cloned().ok_or_else(|| {
                Error::Invalid("preserving metadata requires an existing target".into())
            }),
            Self::Explicit(spec) => spec.resolve(),
        }
    }
}
