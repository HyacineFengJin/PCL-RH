use super::*;
use pixels::Pixels;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .find(|p| p.join("crates/core/Cargo.toml").is_file())
            .unwrap();
        let path = repo
            .join("work/launcher-local-actions-2026-10-05")
            .join(format!(
                "image-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn skin(height: u32) -> Pixels {
    Pixels {
        width: 64,
        height,
        rgba: vec![255; 64 * height as usize * 4],
    }
}
fn paint(image: &mut Pixels, x: u32, y: u32, width: u32, height: u32, rgba: [u8; 4]) {
    for py in y..y + height {
        for px in x..x + width {
            let i = ((py * image.width + px) * 4) as usize;
            image.rgba[i..i + 4].copy_from_slice(&rgba);
        }
    }
}
#[test]
fn head_and_alpha_overlay_are_cropped_and_nearest_resized() {
    let mut image = skin(64);
    paint(&mut image, 8, 8, 8, 8, [200, 40, 20, 0]);
    paint(&mut image, 40, 8, 8, 8, [0, 100, 220, 128]);
    let head = pixels::head(&image, 64).unwrap();
    assert_eq!(&head.rgba[..4], &[100, 70, 120, 255]);
    paint(&mut image, 8, 8, 1, 1, [100, 50, 10, 255]);
    paint(&mut image, 40, 8, 1, 1, [0, 0, 0, 0]);
    let head = pixels::head(&image, 16).unwrap();
    assert_eq!(&head.rgba[..8], &[100, 50, 10, 255, 100, 50, 10, 255]);
    assert_eq!(
        pixels::decode(&pixels::encode(&head).unwrap())
            .unwrap()
            .rgba,
        head.rgba
    );
}
#[test]
fn official_account_previews_crop_actual_head_and_cape_front_pixels() {
    let mut image = skin(64);
    paint(&mut image, 8, 8, 8, 8, [30, 60, 90, 255]);
    paint(&mut image, 40, 8, 8, 8, [0, 0, 0, 0]);
    let (_, avatar) = account_skin(&pixels::encode(&image).unwrap()).unwrap();
    let avatar = pixels::decode(&avatar).unwrap();
    assert_eq!((avatar.width, avatar.height), (64, 64));
    assert_eq!(&avatar.rgba[..4], &[30, 60, 90, 255]);
    let mut cape = skin(32);
    paint(&mut cape, 1, 1, 10, 16, [10, 20, 30, 255]);
    let cape = pixels::decode(&account_cape(&pixels::encode(&cape).unwrap()).unwrap()).unwrap();
    assert_eq!((cape.width, cape.height), (10, 16));
    assert!(cape.rgba.chunks_exact(4).all(|p| p == [10, 20, 30, 255]));
    assert!(account_cape(&pixels::encode(&image).unwrap()).is_err());
    assert!(account_skin(b"not png").is_err());
}
#[test]
fn legacy_opaque_padding_is_not_a_modern_opaque_hat() {
    let mut legacy = skin(32);
    paint(&mut legacy, 8, 8, 8, 8, [90, 20, 10, 255]);
    assert_eq!(
        &pixels::head(&legacy, 8).unwrap().rgba[..4],
        &[90, 20, 10, 255]
    );
    let mut modern = skin(64);
    paint(&mut modern, 8, 8, 8, 8, [90, 20, 10, 255]);
    assert_eq!(
        &pixels::head(&modern, 8).unwrap().rgba[..4],
        &[255, 255, 255, 255]
    );
    paint(&mut legacy, 32, 0, 1, 1, [0, 0, 0, 0]);
    assert_eq!(
        &pixels::head(&legacy, 8).unwrap().rgba[..4],
        &[255, 255, 255, 255]
    );
}
#[test]
fn hd_dimensions_and_size_limits_are_actual_pixels() {
    let hd = Pixels {
        width: 128,
        height: 64,
        rgba: vec![0; 128 * 64 * 4],
    };
    assert_eq!(pixels::skin_dimensions(&hd).unwrap(), 2);
    assert_eq!(pixels::head(&hd, 512).unwrap().rgba.len(), 512 * 512 * 4);
    for size in [0, 7, 513] {
        assert!(pixels::head(&hd, size).is_err());
    }
    assert!(pixels::skin_dimensions(&Pixels {
        width: 65,
        height: 64,
        rgba: Vec::new()
    })
    .is_err());
}
#[test]
fn session_choice_is_stable_and_stale_or_linked_sources_do_not_replace_it() {
    let f = Fixture::new();
    let path = f.0.join("skin.png");
    let bytes = pixels::encode(&skin(64)).unwrap();
    fs::write(&path, &bytes).unwrap();
    let session = ImageSession::default();
    let first = session.begin_choice().unwrap();
    let chosen = session.select(&path, first).unwrap();
    let old = session.begin_choice().unwrap();
    let current = session.begin_choice().unwrap();
    assert!(session.select(&path, old).is_err());
    assert!(session.recheck(&chosen.source_id).is_ok());
    std::os::unix::fs::symlink(&path, f.0.join("link.png")).unwrap();
    assert!(session.select(&f.0.join("link.png"), current).is_err());
    assert!(session.render(&chosen.source_id, 8).is_ok());
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert!(session.render("../../accounts.json", 64).is_err());
}
#[test]
fn invalid_png_and_concat_images_are_rejected_before_saving() {
    assert!(
        sanitize_canvas_png(&base64::engine::general_purpose::STANDARD.encode(b"not png")).is_err()
    );
    let png = pixels::encode(&skin(64)).unwrap();
    let mut doubled = png.clone();
    doubled.extend_from_slice(&png);
    assert!(pixels::decode(&doubled).is_err());
    let mut corrupted = png.clone();
    let end = corrupted.len() - 1;
    corrupted[end] ^= 1;
    assert!(pixels::decode(&corrupted).is_err());
    assert!(sanitize_canvas_png("data:image/png;base64,AAAA").is_err());
}
#[test]
fn animated_and_oversized_decoded_images_are_refused_and_text_is_stripped() {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, 16, 16);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .add_text_chunk(
                "Private annotation".into(),
                "do not export this text".into(),
            )
            .unwrap();
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&vec![255; 16 * 16 * 4]).unwrap();
        writer.finish().unwrap();
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let clean = sanitize_canvas_png(&encoded).unwrap();
    assert!(!clean.windows(4).any(|v| v == b"tEXt"));
    assert!(!clean.windows(23).any(|v| v == b"do not export this text"));
    assert_eq!(pixels::decode(&clean).unwrap().width, 16);
    let mut animated = pixels::encode(&skin(64)).unwrap();
    // APNG recognition happens before any animation control or pixel inflation.
    animated.splice(
        33..33,
        [
            0, 0, 0, 8, b'a', b'c', b'T', b'L', 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0,
        ],
    );
    assert!(pixels::decode(&animated).err().unwrap().contains("静态"));
    let mut oversized = Vec::new();
    {
        let encoder = png::Encoder::new(&mut oversized, 2049, 1);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&vec![0; 2049]).unwrap();
        writer.finish().unwrap();
    }
    assert!(pixels::decode(&oversized).is_err());
    assert!(sanitize_canvas_png(&"A".repeat(((INPUT_LIMIT + 2) / 3) * 4 + 1)).is_err());
}
#[test]
fn save_is_genuine_png_nooverwrite_and_failed_guard_leaves_nothing() {
    let f = Fixture::new();
    let bytes = pixels::encode(&pixels::head(&skin(64), 32).unwrap()).unwrap();
    let path = f.0.join("avatar.png");
    let output = publish::publish_new(&path, &bytes, "png", 1, || Ok(())).unwrap();
    assert_eq!(output.status, "complete");
    assert_eq!(pixels::decode(&fs::read(&path).unwrap()).unwrap().width, 32);
    assert!(publish::publish_new(&path, b"replaced", "png", 1, || Ok(())).is_err());
    assert_eq!(fs::read(&path).unwrap(), bytes);
    let rejected = f.0.join("rejected.png");
    assert!(
        publish::publish_new(&rejected, &bytes, "png", 1, || Err::<(), _>(
            "stale source".into()
        ))
        .is_err()
    );
    assert!(!rejected.exists());
    std::os::unix::fs::symlink(&path, f.0.join("alias.png")).unwrap();
    assert!(publish::publish_new(&f.0.join("alias.png"), &bytes, "png", 1, || Ok(())).is_err());
}
#[test]
fn late_destination_conflict_and_folder_replacement_retain_foreign_data() {
    let f = Fixture::new();
    let bytes = pixels::encode(&pixels::head(&skin(64), 32).unwrap()).unwrap();
    let late = f.0.join("late.png");
    let result = publish::publish_new(&late, &bytes, "png", 1, || {
        fs::write(&late, b"late external file").unwrap();
        Ok(())
    });
    assert!(result.is_err());
    assert_eq!(fs::read(&late).unwrap(), b"late external file");
    let destination = f.0.join("folder");
    fs::create_dir(&destination).unwrap();
    let moved = f.0.join("original-folder");
    assert!(
        publish::publish_new(&destination.join("avatar.png"), &bytes, "png", 1, || {
            fs::rename(&destination, &moved).unwrap();
            fs::create_dir(&destination).unwrap();
            fs::write(destination.join("foreign.txt"), b"retained").unwrap();
            Ok(())
        })
        .is_err()
    );
    assert_eq!(fs::read_dir(moved).unwrap().count(), 0);
    assert_eq!(
        fs::read(destination.join("foreign.txt")).unwrap(),
        b"retained"
    );
}
#[test]
fn export_guard_pins_the_source_against_a_new_choice_until_commit_finishes() {
    use std::sync::{mpsc, Arc};
    use std::time::Duration;
    let f = Fixture::new();
    let path = f.0.join("skin.png");
    fs::write(&path, pixels::encode(&skin(64)).unwrap()).unwrap();
    let session = Arc::new(ImageSession::default());
    let source = session
        .select(&path, session.begin_choice().unwrap())
        .unwrap();
    let guard = session.authorize_export(&source.source_id).unwrap();
    let (started, observed) = mpsc::channel();
    let (finished, result) = mpsc::channel();
    let other = session.clone();
    let worker = std::thread::spawn(move || {
        started.send(()).unwrap();
        other.begin_choice().unwrap();
        finished.send(()).unwrap();
    });
    observed.recv().unwrap();
    assert!(result.recv_timeout(Duration::from_millis(30)).is_err());
    drop(guard);
    result.recv_timeout(Duration::from_secs(2)).unwrap();
    worker.join().unwrap();
}
