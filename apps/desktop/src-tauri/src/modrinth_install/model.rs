use serde::Serialize;
/// Only server-captured target facts enter compatibility checks. This type is
/// deliberately not Deserialize: callers cannot supply a loader/game version.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct Compatibility {
    pub minecraft_version: String,
    pub loader: String,
    pub shader_engines: Vec<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct LocalFile {
    pub kind: String,
    pub file_name: String,
    pub size: u64,
    pub sha512: String,
    pub enabled: bool,
    pub fingerprint: String,
    /// Resource panel v2 token, captured from the same verified descriptor.
    pub resource_fingerprint: String,
}
