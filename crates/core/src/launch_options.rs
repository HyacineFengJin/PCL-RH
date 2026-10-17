//! Typed game defaults shared by persistence and launch planning. They are
//! expanded into argv, never interpreted by a shell. Default values preserve
//! the version's existing arguments; explicit choices replace conflicting flags.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum WindowSize {
    #[default]
    Default,
    Custom {
        width: u32,
        height: u32,
    },
}

impl<'de> Deserialize<'de> for WindowSize {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Serde's internally tagged unit variant ignores extra fields. Use an
        // empty struct variant so future or misspelled settings are retained as
        // invalid data rather than silently accepted and overwritten on save.
        #[derive(Deserialize)]
        #[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
        enum WireWindowSize {
            Default {},
            Custom { width: u32, height: u32 },
        }
        Ok(match WireWindowSize::deserialize(deserializer)? {
            WireWindowSize::Default {} => Self::Default,
            WireWindowSize::Custom { width, height } => Self::Custom { width, height },
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IpPreference {
    #[default]
    Default,
    Ipv4,
    Ipv6,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LaunchOptions {
    pub window: WindowSize,
    pub ip: IpPreference,
    pub window_title: String,
    /// Captured from the registered game's settings. It is not a second copy
    /// of that policy in launcher preference exports.
    #[serde(skip)]
    pub isolation: crate::content_scope::IsolationPolicy,
}
impl LaunchOptions {
    pub fn validate(&self) -> Result<(), String> {
        crate::game_title::validate(&self.window_title)?;
        if let WindowSize::Custom { width, height } = self.window {
            if !(320..=16384).contains(&width) || !(240..=16384).contains(&height) {
                return Err("Window width must be 320–16384 and height 240–16384 pixels".into());
            }
        }
        Ok(())
    }
    pub(crate) fn custom_resolution(&self) -> bool {
        matches!(self.window, WindowSize::Custom { .. })
    }
    pub(crate) fn apply_jvm(&self, args: &mut Vec<String>) {
        if self.ip == IpPreference::Default {
            return;
        }
        args.retain(|arg| {
            !arg.starts_with("-Djava.net.preferIPv4Stack=")
                && !arg.starts_with("-Djava.net.preferIPv6Addresses=")
        });
        // Address preference keeps dual-stack sockets available; choosing IPv4
        // must not accidentally make IPv6-only servers unreachable.
        args.push("-Djava.net.preferIPv4Stack=false".into());
        args.push(format!(
            "-Djava.net.preferIPv6Addresses={}",
            self.ip == IpPreference::Ipv6
        ));
    }
    pub(crate) fn apply_game(&self, args: &mut Vec<String>) {
        let WindowSize::Custom { width, height } = self.window else {
            return;
        };
        let mut result = Vec::with_capacity(args.len() + 4);
        let mut iter = std::mem::take(args).into_iter().peekable();
        while let Some(arg) = iter.next() {
            if matches!(arg.as_str(), "--width" | "--height") {
                if iter.peek().is_some_and(|value| !value.starts_with("--")) {
                    iter.next();
                }
            } else if !arg.starts_with("--width=") && !arg.starts_with("--height=") {
                result.push(arg);
            }
        }
        result.extend([
            "--width".into(),
            width.to_string(),
            "--height".into(),
            height.to_string(),
        ]);
        *args = result;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_preserves_existing_version_arguments() {
        let original = vec![
            "-Djava.net.preferIPv6Addresses=system".into(),
            "--width=900".into(),
        ];
        let mut args = original.clone();
        LaunchOptions::default().apply_jvm(&mut args);
        LaunchOptions::default().apply_game(&mut args);
        assert_eq!(args, original);
    }
    #[test]
    fn explicit_size_replaces_pairs_and_equals_without_consuming_other_options() {
        let mut args = [
            "--width",
            "854",
            "--height=480",
            "--width",
            "--username",
            "Player",
            "--accessToken",
            "private",
        ]
        .map(String::from)
        .to_vec();
        LaunchOptions {
            window: WindowSize::Custom {
                width: 1280,
                height: 720,
            },
            ..Default::default()
        }
        .apply_game(&mut args);
        assert_eq!(
            args,
            [
                "--username",
                "Player",
                "--accessToken",
                "private",
                "--width",
                "1280",
                "--height",
                "720"
            ]
        );
    }
    #[test]
    fn both_address_preferences_remove_conflicts_and_preserve_dual_stack() {
        for (ip, address) in [(IpPreference::Ipv4, "false"), (IpPreference::Ipv6, "true")] {
            let mut args = [
                "-Xmx14G",
                "-Djava.net.preferIPv4Stack=true",
                "-Djava.net.preferIPv6Addresses=system",
                "-cp",
                "fixture",
            ]
            .map(String::from)
            .to_vec();
            LaunchOptions {
                ip,
                ..Default::default()
            }
            .apply_jvm(&mut args);
            assert!(args.contains(&"-Djava.net.preferIPv4Stack=false".into()));
            assert!(args.contains(&format!("-Djava.net.preferIPv6Addresses={address}")));
            assert!(args.contains(&"-Xmx14G".into()));
            assert_eq!(
                args.iter()
                    .filter(|arg| arg.starts_with("-Djava.net.preferIPv"))
                    .count(),
                2
            );
        }
    }
    #[test]
    fn invalid_dimensions_and_unknown_transport_fields_are_rejected() {
        for (width, height) in [
            (0, 720),
            (319, 720),
            (1920, 239),
            (16385, 720),
            (1920, 16385),
        ] {
            assert!(LaunchOptions {
                window: WindowSize::Custom { width, height },
                ..Default::default()
            }
            .validate()
            .is_err());
        }
        for json in [
            r#"{"window":{"mode":"custom","width":1280,"height":720,"shell":"x"}}"#,
            r#"{"window":{"mode":"default","width":1280}}"#,
            r#"{"ip":"ipv7"}"#,
            r#"{"unknown":true}"#,
        ] {
            assert!(
                serde_json::from_str::<LaunchOptions>(json).is_err(),
                "{json}"
            );
        }
    }
}
