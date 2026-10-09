//! Best-effort appearance cache. Authentication success is independent of CDN
//! availability. Only fixed Mojang texture URLs are requested, without bearer
//! tokens or redirects; cached raw PNGs remain bounded and decode on every use.
use crate::{
    launcher_local::filesystem::{Dir, Snapshot},
    toolbox_images,
};
use base64::Engine;
use pcl_auth::{official_texture_url, AccountSummary};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};
const LIMIT: u64 = 1024 * 1024;

#[derive(Clone, Default, Serialize)]
pub struct AccountAppearance {
    pub avatar_png: Option<String>,
    pub skin_png: Option<String>,
    pub cape_png: Option<String>,
    pub cape_name: String,
    pub has_cape: bool,
    pub warning: Option<String>,
}
pub struct AppearanceCache {
    path: PathBuf,
    ready: Mutex<BTreeMap<String, AccountAppearance>>,
}
fn image(bytes: &[u8]) -> String {
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}
impl AppearanceCache {
    pub fn new(project: &Path) -> Self {
        Self {
            path: project.join(".pcl-rust/account-textures"),
            ready: Mutex::new(BTreeMap::new()),
        }
    }
    pub fn load(&self, profile: &AccountSummary, refresh: bool) -> AccountAppearance {
        let key = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&(&profile.id, &profile.textures)).unwrap_or_default()
            )
        );
        // This lock bounds network work to one account at a time, independently
        // of the account-state lock. Reading/login/cancellation stay responsive.
        let mut cache = self.ready.lock().unwrap_or_else(|e| e.into_inner());
        if !refresh {
            if let Some(view) = cache.get(&key) {
                return view.clone();
            }
        }
        let mut view = AccountAppearance::default();
        view.has_cape = profile.textures.cape.is_some();
        if let Some(skin) = &profile.textures.skin {
            match self
                .texture(&skin.url)
                .and_then(|bytes| toolbox_images::account_skin(&bytes))
            {
                Ok((skin, avatar)) => {
                    view.skin_png = Some(image(&skin));
                    view.avatar_png = Some(image(&avatar));
                }
                Err(_) => view.warning = Some("皮肤获取失败，可以刷新重试".into()),
            }
        }
        if let Some(cape) = &profile.textures.cape {
            view.cape_name = cape.name.clone();
            match self
                .texture(&cape.url)
                .and_then(|bytes| toolbox_images::account_cape(&bytes))
            {
                Ok(cape) => view.cape_png = Some(image(&cape)),
                Err(_) => view.warning = Some("披风获取失败，可以刷新重试".into()),
            }
        }
        if cache.len() >= 64 {
            cache.clear();
        }
        cache.insert(key, view.clone());
        view
    }
    fn texture(&self, value: &str) -> Result<Vec<u8>, String> {
        let url = official_texture_url(value).ok_or("不是官方皮肤地址")?;
        let name = format!("{:x}.png", Sha256::digest(url.as_bytes()));
        let folder = Dir::create_absolute(&self.path)?;
        folder.require_private()?;
        let _writer = folder.lock(".textures.lock")?;
        let mut previous = None;
        if folder.file(&name)?.is_some() {
            let mut snapshot = Snapshot::open(&folder, &name, LIMIT)?;
            let bytes = snapshot.bytes(&folder, &name)?;
            if toolbox_images::account_skin(&bytes).is_ok()
                || toolbox_images::account_cape(&bytes).is_ok()
            {
                return Ok(bytes);
            }
            previous = Some(snapshot);
        }
        let http = pcl_network::blocking_client()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(12))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "无法初始化皮肤下载")?;
        let response = http.get(url).send().map_err(|_| "无法连接皮肤服务")?;
        if !response.status().is_success() || response.content_length().is_some_and(|n| n > LIMIT) {
            return Err("皮肤服务响应无效".into());
        }
        let mut bytes = Vec::new();
        response
            .take(LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "皮肤下载未完成")?;
        if bytes.len() as u64 > LIMIT || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err("皮肤文件无效或超过大小上限".into());
        }
        // Decode before publishing so an incomplete response never becomes a
        // reusable cache entry. Both supported shapes fit within the same bound.
        if toolbox_images::account_skin(&bytes).is_err()
            && toolbox_images::account_cape(&bytes).is_err()
        {
            return Err("皮肤像素数据无效".into());
        }
        folder.replace(&name, previous.as_ref(), &bytes, LIMIT)?;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcl_auth::{ProfileTextures, TextureReference};
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../work/auth-appearance-tests")
                .join(format!(
                    "{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn profile(skin: &str, cape: Option<&str>) -> AccountSummary {
        AccountSummary {
            id: "account-fixture".into(),
            uuid: "0".repeat(32),
            name: "OnlinePlayer".into(),
            expires_at: 0,
            textures: ProfileTextures {
                skin: Some(TextureReference {
                    url: skin.into(),
                    name: String::new(),
                }),
                cape: cape.map(|url| TextureReference {
                    url: url.into(),
                    name: "Cape".into(),
                }),
            },
        }
    }
    fn png(height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 64, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer
                .write_image_data(&vec![255; 64 * height as usize * 4])
                .unwrap();
            writer.finish().unwrap();
        }
        bytes
    }
    #[test]
    fn cached_public_textures_render_offline_and_keep_secrets_out_of_the_view() {
        let f = Fixture::new();
        let cache = AppearanceCache::new(&f.0);
        let skin = format!("https://textures.minecraft.net/texture/{}", "a".repeat(64));
        let cape = format!("https://textures.minecraft.net/texture/{}", "b".repeat(64));
        let folder = Dir::create_absolute(&cache.path).unwrap();
        for (url, bytes) in [(&skin, png(64)), (&cape, png(32))] {
            let name = format!("{:x}.png", Sha256::digest(url.as_bytes()));
            folder.replace(&name, None, &bytes, LIMIT).unwrap();
        }
        let public = profile(&skin, Some(&cape));
        for refresh in [false, true] {
            let view = cache.load(&public, refresh);
            assert!(view.warning.is_none());
            assert!(view.has_cape);
            assert_eq!(view.cape_name, "Cape");
            for (data, width, height) in [
                (view.avatar_png.unwrap(), 64u32, 64u32),
                (view.cape_png.unwrap(), 10u32, 16u32),
            ] {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(data.strip_prefix("data:image/png;base64,").unwrap())
                    .unwrap();
                assert_eq!(&bytes[16..20], &width.to_be_bytes());
                assert_eq!(&bytes[20..24], &height.to_be_bytes());
            }
        }
    }
    #[test]
    fn foreign_urls_and_linked_cache_files_are_rejected_without_network_or_overwrite() {
        let f = Fixture::new();
        let cache = AppearanceCache::new(&f.0);
        assert!(cache.texture("https://example.invalid/skin.png").is_err());
        assert!(!cache.path.exists());
        let url = format!("https://textures.minecraft.net/texture/{}", "c".repeat(64));
        let folder = Dir::create_absolute(&cache.path).unwrap();
        let name = format!("{:x}.png", Sha256::digest(url.as_bytes()));
        let original = f.0.join("original.png");
        let bytes = png(64);
        std::fs::write(&original, &bytes).unwrap();
        std::os::unix::fs::symlink(&original, cache.path.join(&name)).unwrap();
        assert!(cache.texture(&url).is_err());
        let view = cache.load(&profile(&url, None), false);
        assert!(view.avatar_png.is_none());
        assert!(view.warning.is_some());
        assert_eq!(std::fs::read(&original).unwrap(), bytes);
        assert!(folder.file(&name).is_err());
    }
}
