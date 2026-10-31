type Result<T> = std::result::Result<T, String>;
pub(crate) fn file_name(value: &str) -> Result<()> {
    pcl_core::identifier(value)?;
    if value.len() > 255 || value.trim() != value || value.chars().any(char::is_control) {
        return Err("Modrinth文件名无效".into());
    }
    Ok(())
}
