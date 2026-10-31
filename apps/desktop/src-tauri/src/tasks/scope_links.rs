//! Private directory alias fixtures for the real scheduler tests. Junctions
//! need neither developer mode nor administrator/symlink privilege.
#[path = "../../../../../crates/core/src/file_ops/junction_fixture.rs"]
#[allow(dead_code)]
mod junction_fixture;
pub(crate) fn symlink(
    target: impl AsRef<std::path::Path>,
    alias: impl AsRef<std::path::Path>,
) -> std::io::Result<()> {
    std::fs::create_dir(alias.as_ref())?;
    junction_fixture::set_existing(target.as_ref(), alias.as_ref())
}
