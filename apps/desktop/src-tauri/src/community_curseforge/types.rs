use serde::Deserialize;

#[derive(Deserialize)]
pub(super) struct Envelope<T> {
    pub data: T,
}

#[derive(Deserialize)]
pub(super) struct Page<T> {
    pub data: Vec<T>,
    pub pagination: Pagination,
}

impl<T> Page<T> {
    pub fn validate(&self, index: usize, page_size: usize) -> Result<(), String> {
        let page = &self.pagination;
        if page.index != index as u64
            || page.page_size != page_size as u64
            || page.result_count != self.data.len() as u64
            || self.data.len() > page_size
            || page.result_count > page.total_count.saturating_sub(page.index)
            || (page.result_count == 0 && page.total_count > page.index)
        {
            return Err("CurseForge 分页响应无效".into());
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Pagination {
    pub index: u64,
    pub page_size: u64,
    pub result_count: u64,
    pub total_count: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Project {
    pub id: u32,
    pub game_id: u32,
    pub name: String,
    pub slug: String,
    pub links: Links,
    pub summary: String,
    pub download_count: u64,
    #[serde(default)]
    pub categories: Vec<Category>,
    pub class_id: Option<u32>,
    #[serde(default)]
    pub logo: Option<Asset>,
    #[serde(default)]
    pub latest_files_indexes: Vec<FileIndex>,
    pub date_modified: String,
    pub allow_mod_distribution: Option<bool>,
    pub is_available: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Links {
    pub website_url: String,
}

#[derive(Deserialize)]
pub(super) struct Category {
    pub slug: String,
}

#[derive(Deserialize)]
pub(super) struct Asset {
    pub url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct FileIndex {
    pub game_version: String,
    pub mod_loader: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct File {
    pub id: u32,
    pub game_id: u32,
    pub mod_id: u32,
    pub is_available: bool,
    pub display_name: String,
    pub file_name: String,
    pub release_type: u32,
    #[serde(default)]
    pub hashes: Vec<Hash>,
    pub file_date: String,
    pub file_length: u64,
    pub download_count: u64,
    pub download_url: Option<String>,
    #[serde(default)]
    pub game_versions: Vec<String>,
    #[serde(default)]
    pub dependencies: Vec<Dependency>,
    #[serde(default)]
    pub is_early_access_content: bool,
}

impl File {
    pub fn check_owner(&self, project_id: u32) -> Result<(), String> {
        if self.id == 0 || self.game_id != super::MINECRAFT_GAME || self.mod_id != project_id {
            return Err("CurseForge 文件 ID、游戏或项目归属不匹配".into());
        }
        Ok(())
    }
}

#[derive(Deserialize)]
pub(super) struct Hash {
    pub value: String,
    pub algo: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Dependency {
    pub mod_id: u32,
    pub relation_type: u32,
}
