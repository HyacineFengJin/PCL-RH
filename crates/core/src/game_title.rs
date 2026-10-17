//! Optional, in-process LWJGL title customization. The fixed Java 8 agent changes
//! only title parameters on the game's own thread; it neither edits game JARs
//! nor asks a window manager to control unrelated windows. Modern GLFW string
//! overloads and legacy Display.setTitle are supported. Native pointer/buffer
//! calls and mods bypassing these APIs retain their own titles.
//!
//! Preparation materializes a checked, immutable agent in launcher storage. The
//! cache contains no title or account data and may survive cancelled preparation;
//! only the existing launch owner may spawn the resulting plan. No Java compiler,
//! helper process, network access, or background title polling runs here.
use std::path::Path;

#[path = "game_title_cache.rs"]
mod cache;

const AGENT: &[u8] = include_bytes!("../assets/game-title-agent.jar");

/// Empty means the game's normal title. Other values are literal Unicode text;
/// rejecting controls also prevents log lines or desktop control sequences from
/// being introduced through this cosmetic setting.
pub fn validate(title: &str) -> Result<(), String> {
    if title.chars().count() > 256 || title.chars().any(char::is_control) {
        return Err("游戏窗口标题最多 256 个字符，且不能包含换行或控制字符".into());
    }
    Ok(())
}

/// Append these arguments before the main class. Calling with an empty title is
/// a pure no-op, preserving default CLI and launch behavior, including no I/O.
pub fn prepare_jvm_args(project: &Path, title: &str) -> Result<Vec<String>, String> {
    validate(title)?;
    if title.is_empty() {
        return Ok(Vec::new());
    }
    let cache = cache::Cache::open(project)?;
    // Java splits -javaagent at the first '='; escaping it is not specified.
    // Fail explicitly instead of loading an unintended path or losing the title.
    cache
        .path()
        .to_str()
        .filter(|path| !path.contains('='))
        .ok_or("启动器路径包含不受 Java agent 支持的字符，无法设置游戏窗口标题")?;
    let path = cache.ensure(&format!("{:x}.jar", md5::compute(AGENT)), AGENT)?;
    let encoded = title
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(vec![format!("-javaagent:{}={encoded}", path.display())])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fixture() -> tempfile::TempDir {
        let work = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../work/game-title-agent");
        fs::create_dir_all(&work).unwrap();
        tempfile::tempdir_in(work).unwrap()
    }

    #[test]
    fn default_does_not_even_require_a_project_directory() {
        assert!(prepare_jvm_args(Path::new("nonexistent-title-fixture"), "")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn unicode_is_one_literal_argument_and_cache_contains_only_fixed_bytes() {
        let fixture = fixture();
        let title = "你好 Minecraft 🦀 \\\" $(literal)";
        let args = prepare_jvm_args(fixture.path(), title).unwrap();
        assert_eq!(args.len(), 1);
        let (path, encoded) = args[0]
            .strip_prefix("-javaagent:")
            .unwrap()
            .split_once('=')
            .unwrap();
        assert_eq!(fs::read(path).unwrap(), AGENT);
        assert!(!encoded.contains(title));
        let decoded = (0..encoded.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&encoded[at..at + 2], 16).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(String::from_utf8(decoded).unwrap(), title);
        assert_eq!(
            prepare_jvm_args(fixture.path(), "changed").unwrap()[0]
                .split_once('=')
                .unwrap()
                .0,
            args[0].split_once('=').unwrap().0
        );
    }

    #[test]
    fn unknown_cache_bytes_are_rejected_and_never_replaced() {
        let fixture = fixture();
        let cache = cache::Cache::open(fixture.path()).unwrap();
        let path = cache.path().join(format!("{:x}.jar", md5::compute(AGENT)));
        fs::write(&path, "untrusted").unwrap();
        assert!(prepare_jvm_args(fixture.path(), "custom").is_err());
        assert_eq!(fs::read(path).unwrap(), b"untrusted");
    }

    #[test]
    fn validation_limits_unicode_characters_and_rejects_controls_before_io() {
        assert!(validate(&"🦀".repeat(256)).is_ok());
        for title in [
            "🦀".repeat(257),
            "line\nbreak".into(),
            "null\0title".into(),
            "\u{7f}".into(),
        ] {
            assert!(prepare_jvm_args(Path::new("nonexistent-title-fixture"), &title).is_err());
        }
    }

    #[test]
    fn equals_in_agent_path_fails_instead_of_changing_java_option_semantics() {
        let fixture = fixture();
        let project = fixture.path().join("literal=project");
        fs::create_dir(&project).unwrap();
        assert!(prepare_jvm_args(&project, "custom")
            .unwrap_err()
            .contains("Java agent"));
    }

    #[cfg(unix)]
    #[test]
    fn cache_symlinks_are_rejected_without_writing_their_target() {
        use std::os::unix::fs::symlink;
        let fixture = fixture();
        let outside = fixture.path().join("outside");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, fixture.path().join(".pcl-rust")).unwrap();
        assert!(prepare_jvm_args(fixture.path(), "custom").is_err());
        assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn even_a_symlink_to_the_exact_agent_bytes_is_rejected() {
        use std::os::unix::fs::symlink;
        let fixture = fixture();
        let cache = cache::Cache::open(fixture.path()).unwrap();
        let original = fixture.path().join("original.jar");
        fs::write(&original, AGENT).unwrap();
        let path = cache.path().join(format!("{:x}.jar", md5::compute(AGENT)));
        symlink(&original, &path).unwrap();
        assert!(prepare_jvm_args(fixture.path(), "custom").is_err());
        assert_eq!(fs::read(original).unwrap(), AGENT);
        assert!(fs::symlink_metadata(path).unwrap().file_type().is_symlink());
    }

    #[test]
    fn concurrent_preparations_share_verified_bytes_and_leave_no_temporary_files() {
        let fixture = fixture();
        std::thread::scope(|scope| {
            let threads = (0..8)
                .map(|_| {
                    let path = fixture.path();
                    scope.spawn(move || prepare_jvm_args(path, "shared"))
                })
                .collect::<Vec<_>>();
            for thread in threads {
                thread.join().unwrap().unwrap();
            }
        });
        let cache = cache::Cache::open(fixture.path()).unwrap();
        assert_eq!(fs::read_dir(cache.path()).unwrap().count(), 1);
    }
}
