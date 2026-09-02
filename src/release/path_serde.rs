use serde::{Serialize as _, Serializer};
use std::path::{Path, PathBuf};

struct PortablePath<'a>(&'a Path);

impl serde::Serialize for PortablePath<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&crate::utils::path_to_git_format(self.0))
    }
}

pub(super) fn serialize<S>(path: &Path, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    PortablePath(path).serialize(serializer)
}

pub(super) fn serialize_vec<S>(paths: &[PathBuf], serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.collect_seq(paths.iter().map(|path| PortablePath(path)))
}
