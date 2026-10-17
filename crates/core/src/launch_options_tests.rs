//! End-to-end argv planning uses only an explicit executable fixture, never
//! system Java or a Minecraft process. Both official argument formats and
//! authentication share the same options and secret-redaction boundary.
use super::*;
use launch_options::IpPreference;
use serde_json::json;
use std::os::unix::fs::PermissionsExt;

#[test]
fn configured_legacy_and_conditional_plans_keep_jvm_flags_before_main_and_tokens_private() {
    let work = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../work/core-launch-options-tests");
    fs::create_dir_all(&work).unwrap();
    let fixture = tempfile::tempdir_in(work).unwrap();
    let root = fixture.path().join("minecraft");
    let version = root.join("versions/example");
    fs::create_dir_all(&version).unwrap();
    let java = fixture.path().join("java");
    fs::write(&java, format!("#!/bin/sh\nprintf 'java.specification.version = 21\\njava.vendor = Fixture\\nos.arch = {}\\n' >&2\n", std::env::consts::ARCH)).unwrap();
    fs::set_permissions(&java, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(version.join("example.jar"), []).unwrap();
    let selection = java::JavaSelection::Manual {
        path: java.display().to_string(),
    };
    let identity = OnlineIdentity {
        name: "Player".into(),
        uuid: "a".repeat(32),
        access_token: "fixture-private-token".into(),
        xuid: "123".into(),
        client_id: "fixture-client".into(),
    };
    let options = LaunchOptions {
        window: WindowSize::Custom {
            width: 1280,
            height: 720,
        },
        ip: IpPreference::Ipv6,
        window_title: "自定义测试 🦀".into(),
        ..Default::default()
    };
    for conditional in [false, true] {
        let mut data = json!({
            "id": "example",
            "mainClass": "FixtureMain",
            "javaVersion": {"majorVersion": 21},
            "libraries": [],
            "arguments": {
                "jvm": [
                    "-Djava.net.preferIPv4Stack=true",
                    "-Djava.net.preferIPv6Addresses=false"
                ],
                "game": [
                    "--accessToken", "${auth_access_token}",
                    {
                        "rules": [{"action": "allow", "features": {"has_custom_resolution": true}}],
                        "value": ["--width", "${resolution_width}", "--height", "${resolution_height}"]
                    }
                ]
            }
        });
        if !conditional {
            data.as_object_mut().unwrap().remove("arguments");
            data["minecraftArguments"] =
                "--accessToken ${auth_access_token} --width 854 --height=480".into();
        }
        fs::write(version.join("example.json"), data.to_string()).unwrap();
        let default = build_launch_plan_with_java(
            &root,
            fixture.path(),
            "example",
            "Player",
            14,
            &selection,
            &[],
        )
        .unwrap();
        assert!(!default
            .args
            .iter()
            .any(|arg| arg.starts_with("-javaagent:")));
        if !conditional {
            assert!(!fixture.path().join(".pcl-rust/game-title-agent").exists());
        }
        if conditional {
            assert!(!default
                .args
                .iter()
                .any(|arg| arg == "--width" || arg == "--height"));
            assert!(default
                .args
                .contains(&"-Djava.net.preferIPv4Stack=true".into()));
        } else {
            assert!(default
                .args
                .windows(2)
                .any(|pair| pair == ["--width", "854"]));
            assert!(default.args.contains(&"--height=480".into()));
        }
        let plan = build_launch_plan_authenticated_with_options(
            &root,
            fixture.path(),
            "example",
            &identity,
            14,
            &selection,
            &[],
            &options,
        )
        .unwrap();
        let main = plan
            .args
            .iter()
            .position(|arg| arg == "FixtureMain")
            .unwrap();
        assert!(plan.args[..main].contains(&"-Xmx14G".into()));
        assert!(plan.args[..main].contains(&"-Djava.net.preferIPv4Stack=false".into()));
        assert!(plan.args[..main].contains(&"-Djava.net.preferIPv6Addresses=true".into()));
        assert_eq!(
            plan.args
                .iter()
                .filter(|arg| arg.starts_with("-javaagent:"))
                .count(),
            1
        );
        assert!(plan.args[..main]
            .iter()
            .any(|arg| arg.starts_with("-javaagent:")));
        assert!(plan.args[main + 1..]
            .windows(2)
            .any(|pair| pair == ["--width", "1280"]));
        assert!(plan.args[main + 1..]
            .windows(2)
            .any(|pair| pair == ["--height", "720"]));
        assert_eq!(plan.args.iter().filter(|arg| *arg == "--width").count(), 1);
        assert!(plan.args.contains(&identity.access_token));
        assert!(!serde_json::to_string(&plan)
            .unwrap()
            .contains(&identity.access_token));
        assert_eq!(plan.game_dir, root.canonicalize().unwrap());
        let offline = build_launch_plan_with_options(
            &root,
            fixture.path(),
            "example",
            "Player",
            14,
            &selection,
            &[],
            &options,
        )
        .unwrap();
        assert!(offline.args.contains(&"0".into()));
        assert!(!offline.args.contains(&identity.access_token));
    }
}
