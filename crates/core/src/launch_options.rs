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
    /// Replaces the version metadata's `${version_type}` value. An empty value
    /// preserves that metadata; this is independent of the game window title.
    pub custom_info: String,
    pub jvm_args: String,
    pub game_args: String,
    /// Captured from the registered game's settings. It is not a second copy
    /// of that policy in launcher preference exports.
    #[serde(skip)]
    pub isolation: crate::content_scope::IsolationPolicy,
}

/// Per-instance overrides distinguish inheritance from an explicit empty value.
/// Keeping the text unchanged also lets the editor round-trip quotes and paths.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct LaunchArgumentOverrides {
    pub custom_info: Option<String>,
    pub jvm_args: Option<String>,
    pub game_args: Option<String>,
}

impl LaunchArgumentOverrides {
    pub fn validate(&self) -> Result<(), String> {
        if let Some(info) = &self.custom_info {
            validate_custom_info(info)?;
        }
        if let Some(args) = &self.jvm_args {
            parse_jvm_args(args)?;
        }
        if let Some(args) = &self.game_args {
            parse_game_args(args)?;
        }
        Ok(())
    }

    pub fn apply_to(&self, options: &mut LaunchOptions) -> Result<(), String> {
        // Validation precedes publication: a malformed override must not leave
        // a partially updated policy behind when a caller reuses the snapshot.
        let mut updated = options.clone();
        if let Some(info) = &self.custom_info {
            updated.custom_info.clone_from(info);
        }
        if let Some(args) = &self.jvm_args {
            updated.jvm_args.clone_from(args);
        }
        if let Some(args) = &self.game_args {
            updated.game_args.clone_from(args);
        }
        updated.validate()?;
        *options = updated;
        Ok(())
    }
}

pub const MAX_ARGUMENT_INPUT_BYTES: usize = 16 * 1024;
pub const MAX_ARGUMENT_COUNT: usize = 128;
pub const MAX_ARGUMENT_BYTES: usize = 4 * 1024;
pub const MAX_CUSTOM_INFO_CHARS: usize = 256;

fn validate_custom_info(info: &str) -> Result<(), String> {
    if info.chars().count() > MAX_CUSTOM_INFO_CHARS || info.chars().any(char::is_control) {
        return Err("Custom version information must contain at most 256 characters and no control characters".into());
    }
    Ok(())
}

/// This deliberately is not shell syntax. Quotes can concatenate with unquoted
/// text, and a backslash escapes whitespace, either quote or another backslash.
/// Other backslashes remain literal, so ordinary Windows paths survive. Dollar
/// signs, wildcards, command syntax and version placeholders have no meaning.
/// Tabs/newlines are allowed only as separators, never inside the resulting argv.
fn tokenize(input: &str, kind: &str) -> Result<Vec<String>, String> {
    let invalid = |reason: &str| format!("Invalid {kind} arguments: {reason}");
    if input.len() > MAX_ARGUMENT_INPUT_BYTES {
        return Err(invalid("input exceeds 16 KiB"));
    }
    let mut result = Vec::new();
    let mut token = String::new();
    let mut started = false;
    let mut quote = None;
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch.is_control() && !matches!(ch, '\t' | '\n') {
            return Err(invalid("control characters are not allowed"));
        }
        if ch == '\\' {
            started = true;
            if chars
                .peek()
                .is_some_and(|next| next.is_whitespace() || matches!(next, '\'' | '"' | '\\'))
            {
                let next = chars.next().unwrap();
                if next.is_control() {
                    return Err(invalid("tabs and newlines must separate arguments"));
                }
                token.push(next);
            } else {
                token.push(ch);
            }
        } else if let Some(delimiter) = quote {
            if ch == delimiter {
                quote = None;
            } else if ch.is_control() {
                return Err(invalid("tabs and newlines must separate arguments"));
            } else {
                token.push(ch);
            }
        } else if matches!(ch, '\'' | '"') {
            started = true;
            quote = Some(ch);
        } else if ch.is_whitespace() {
            if started {
                push_token(&mut result, &mut token, kind)?;
                started = false;
            }
        } else {
            started = true;
            token.push(ch);
        }
        if token.len() > MAX_ARGUMENT_BYTES {
            return Err(invalid("an argument exceeds 4 KiB"));
        }
    }
    if quote.is_some() {
        return Err(invalid("a quote is not closed"));
    }
    if started {
        push_token(&mut result, &mut token, kind)?;
    }
    Ok(result)
}

fn push_token(result: &mut Vec<String>, token: &mut String, kind: &str) -> Result<(), String> {
    if result.len() == MAX_ARGUMENT_COUNT {
        return Err(format!("Invalid {kind} arguments: more than 128 arguments"));
    }
    result.push(std::mem::take(token));
    Ok(())
}

fn parse_jvm_args(input: &str) -> Result<Vec<String>, String> {
    let args = tokenize(input, "JVM")?;
    for arg in &args {
        if arg.starts_with('@') {
            return Err("JVM argument files are not allowed".into());
        }
        if !arg.starts_with('-') || matches!(arg.as_str(), "-" | "--") {
            return Err("Each additional JVM argument must be an option; use an equals sign for option values".into());
        }
        let name = arg.split('=').next().unwrap().to_ascii_lowercase();
        if matches!(
            name.as_str(),
            "-cp"
                | "-classpath"
                | "--class-path"
                | "-jar"
                | "-m"
                | "--module"
                | "-p"
                | "--module-path"
                | "--upgrade-module-path"
                | "--patch-module"
                | "--source"
                | "--boot-class-path"
                | "--disable-@files"
                | "--dry-run"
                | "--validate-modules"
                | "--list-modules"
                | "--describe-module"
                | "-d"
        ) || name.starts_with("-xbootclasspath")
            || name.starts_with("-xoptionsfile")
            || name.starts_with("-xx:flags")
            || name.starts_with("-xx:vmoptionsfile")
        {
            return Err(
                "The launcher manages the JVM entry point, classpath and option files".into(),
            );
        }
        if [
            "-djava.class.path",
            "-dsun.boot.class.path",
            "-djdk.module.path",
            "-djdk.module.upgrade.path",
            "-djdk.module.main",
            "-djdk.module.main.class",
        ]
        .contains(&name.as_str())
        {
            return Err("The launcher manages JVM classpath and module properties".into());
        }
        // Heap size has a single owner: the launcher's memory setting. Stack
        // sizing and unrelated collector/tuning options remain user controlled.
        let heap_option = name.strip_prefix("-xx:").unwrap_or("");
        if name.starts_with("-xmx")
            || name.starts_with("-xms")
            || name.starts_with("-xmn")
            || name.starts_with("-mx")
            || name.starts_with("-ms")
            || name.starts_with("-xsoftmx")
            || name.starts_with("-xmos")
            || name.starts_with("-xmox")
            || matches!(
                heap_option,
                "maxheapsize"
                    | "initialheapsize"
                    | "minheapsize"
                    | "softmaxheapsize"
                    | "newsize"
                    | "maxnewsize"
                    | "oldsize"
                    | "maxram"
                    | "maxrampercentage"
                    | "initialrampercentage"
                    | "minrampercentage"
                    | "maxramfraction"
                    | "minramfraction"
                    | "initialramfraction"
                    | "defaultmaxramfraction"
                    | "ergoheapsizelimit"
            )
        {
            return Err(
                "Additional JVM arguments cannot override the launcher's memory setting".into(),
            );
        }
    }
    Ok(args)
}

fn game_option_name(arg: &str) -> Option<&str> {
    let spelling = arg.split('=').next().unwrap();
    spelling
        .strip_prefix("--")
        .or_else(|| spelling.strip_prefix('-'))
        .filter(|name| !name.is_empty())
}

fn managed_window_argument(arg: &str) -> bool {
    game_option_name(arg).is_some_and(|name| {
        let name = name.to_ascii_lowercase();
        "width".starts_with(&name) || "height".starts_with(&name)
    })
}

fn parse_game_args(input: &str) -> Result<Vec<String>, String> {
    let args = tokenize(input, "game")?;
    for arg in &args {
        if arg.starts_with('@') {
            return Err("Game argument files are not allowed".into());
        }
        // jopt-simple also interprets single-hyphen clusters and attached values
        // (for example, an abbreviated option followed immediately by its value).
        // Require the unambiguous long-option spelling for additional options;
        // ordinary negative numeric values and the single '-' value remain valid.
        if arg.starts_with('-')
            && !arg.starts_with("--")
            && arg != "-"
            && !arg.parse::<f64>().is_ok_and(f64::is_finite)
        {
            return Err("Additional game options must use two hyphens; single-hyphen options and clusters are not supported".into());
        }
        let Some(name) = game_option_name(arg) else {
            continue;
        };
        // jopt-simple defaults to unique-prefix matching (OptionParser 5.0.4).
        // Do not depend on whether
        // a prefix is ambiguous in one version: loaders can register a different
        // option set. Only option names participate, never ordinary value tokens.
        // Upstream: https://jopt-simple.github.io/jopt-simple/apidocs/joptsimple/OptionParser.html
        let name = name.to_ascii_lowercase();
        if [
            "username",
            "uuid",
            "accesstoken",
            "gamedir",
            "assetsdir",
            "assetindex",
            "usertype",
            "userproperties",
            "xuid",
            "clientid",
            "version",
            "versiontype",
            "session",
        ]
        .iter()
        .any(|protected| protected.starts_with(&name))
        {
            return Err(
                "Additional game arguments cannot override account, version or directory settings"
                    .into(),
            );
        }
    }
    Ok(args)
}

impl LaunchOptions {
    pub fn validate(&self) -> Result<(), String> {
        crate::game_title::validate(&self.window_title)?;
        validate_custom_info(&self.custom_info)?;
        parse_jvm_args(&self.jvm_args)?;
        parse_game_args(&self.game_args)?;
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
    pub(crate) fn apply_jvm(&self, args: &mut Vec<String>) -> Result<(), String> {
        args.extend(parse_jvm_args(&self.jvm_args)?);
        if self.ip == IpPreference::Default {
            return Ok(());
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
        Ok(())
    }
    pub(crate) fn apply_game(&self, args: &mut Vec<String>) -> Result<(), String> {
        args.extend(parse_game_args(&self.game_args)?);
        let WindowSize::Custom { width, height } = self.window else {
            return Ok(());
        };
        let mut result = Vec::with_capacity(args.len() + 4);
        let mut iter = std::mem::take(args).into_iter().peekable();
        while let Some(arg) = iter.next() {
            // Remove the same single-hyphen and prefix spellings accepted by
            // jopt-simple; leaving an alias creates duplicate option values,
            // rather than allowing the managed size to take priority.
            if managed_window_argument(&arg) {
                if !arg.contains('=')
                    && iter.peek().is_some_and(|value| {
                        !value.starts_with('-') || value.parse::<i64>().is_ok()
                    })
                {
                    iter.next();
                }
            } else {
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
        Ok(())
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
        LaunchOptions::default().apply_jvm(&mut args).unwrap();
        LaunchOptions::default().apply_game(&mut args).unwrap();
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
        .apply_game(&mut args)
        .unwrap();
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
            .apply_jvm(&mut args)
            .unwrap();
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

    #[test]
    fn tokenizer_supports_quote_concatenation_empty_values_and_windows_paths() {
        assert_eq!(
            tokenize(
                r#""" '' pre"quoted "'text'post "C:\Program Files\Java\bin" C:\Games\MC escaped\ value \"quoted\" "\\" $HOME ${version_type} $(whoami) *.jar"#,
                "game"
            )
            .unwrap(),
            [
                "", "", "prequoted textpost", r"C:\Program Files\Java\bin",
                r"C:\Games\MC", "escaped value", "\"quoted\"", r"\", "$HOME",
                "${version_type}", "$(whoami)", "*.jar",
            ]
        );
        assert_eq!(
            tokenize("first\t\n second", "game").unwrap(),
            ["first", "second"]
        );
        assert_eq!(tokenize("tail\\", "game").unwrap(), ["tail\\"]);
    }

    #[test]
    fn tokenizer_bounds_input_tokens_and_output_without_echoing_values() {
        assert!(tokenize(&" ".repeat(MAX_ARGUMENT_INPUT_BYTES), "game").is_ok());
        assert!(tokenize(&" ".repeat(MAX_ARGUMENT_INPUT_BYTES + 1), "game").is_err());
        assert!(tokenize(&"🦀".repeat(MAX_ARGUMENT_BYTES / 4), "game").is_ok());
        assert!(tokenize(&"🦀".repeat(MAX_ARGUMENT_BYTES / 4 + 1), "game").is_err());
        assert_eq!(
            tokenize(&vec!["''"; MAX_ARGUMENT_COUNT].join(" "), "game")
                .unwrap()
                .len(),
            MAX_ARGUMENT_COUNT
        );
        assert!(tokenize(&vec!["''"; MAX_ARGUMENT_COUNT + 1].join(" "), "game").is_err());
        for input in [
            "private-value'",
            "\"private-value",
            "private-value\0",
            "private-value\r",
            "private-value\u{0085}",
            "'private-value\n'",
            "private-value\\\t",
        ] {
            let error = tokenize(input, "game").unwrap_err();
            assert!(!error.contains("private-value"));
        }
    }

    #[test]
    fn jvm_tuning_is_appended_with_managed_ip_preference_last() {
        let options = LaunchOptions {
            ip: IpPreference::Ipv6,
            jvm_args: r#"-Dpath="C:\Program Files\Java" -Dempty="" -javaagent:"C:\Mods\agent.jar"=setting -XX:+UseG1GC --add-opens=java.base/java.lang=ALL-UNNAMED -Xss2m -Djava.net.preferIPv6Addresses=false"#.into(),
            ..Default::default()
        };
        let mut args = vec!["-Xmx4G".into(), "-cp".into(), "version-classpath".into()];
        options.apply_jvm(&mut args).unwrap();
        assert_eq!(&args[..3], ["-Xmx4G", "-cp", "version-classpath"]);
        assert!(args.contains(&r"-Dpath=C:\Program Files\Java".into()));
        assert!(args.contains(&"-Dempty=".into()));
        assert!(args.contains(&r"-javaagent:C:\Mods\agent.jar=setting".into()));
        assert_eq!(args.last().unwrap(), "-Djava.net.preferIPv6Addresses=true");
        assert!(!args.contains(&"-Djava.net.preferIPv6Addresses=false".into()));
    }

    #[test]
    fn jvm_entrypoint_argfiles_and_heap_size_remain_launcher_owned() {
        for input in [
            "private-value",
            "''",
            "--",
            "@private-value",
            "-cp private-value",
            "--class-path=private-value",
            "-classpath=private-value",
            "-jar=private-value",
            "-m=private-value",
            "--module=private-value",
            "-p=private-value",
            "--module-path=private-value",
            "--upgrade-module-path=private-value",
            "--patch-module=private-value",
            "--source=21",
            "-Xbootclasspath/a:private-value",
            "-Djava.class.path=private-value",
            "-Djdk.module.main=private-value",
            "-XX:VMOptionsFile=private-value",
            "-XX:Flags=private-value",
            "-Xmx8G",
            "-Xms2G",
            "-Xmn1G",
            "-mx8G",
            "-ms2G",
            "-Xsoftmx2G",
            "-Xmos2G",
            "-Xmox8G",
            "-XX:MaxHeapSize=8G",
            "-XX:InitialHeapSize=2G",
            "-XX:SoftMaxHeapSize=8G",
            "-XX:MaxRAMPercentage=80",
        ] {
            let mut args = vec!["original".into()];
            let error = LaunchOptions {
                jvm_args: input.into(),
                ..Default::default()
            }
            .apply_jvm(&mut args)
            .unwrap_err();
            assert_eq!(args, ["original"], "{input}");
            assert!(!error.contains("private-value"), "{input}");
        }
    }

    #[test]
    fn game_account_version_and_directory_flags_are_protected_after_tokenization() {
        for flag in [
            "--username",
            "--uuid",
            "--accessToken",
            "--gameDir",
            "--assetsDir",
            "--assetIndex",
            "--userType",
            "--userProperties",
            "--xuid",
            "--clientId",
            "--version",
            "--versionType",
            "--session",
        ] {
            for input in [
                format!("{flag} private-value"),
                format!("{}=private-value", flag.to_uppercase()),
            ] {
                let error = parse_game_args(&input).unwrap_err();
                assert!(!error.contains("private-value"));
            }
        }
        assert!(parse_game_args("--user'name'=private-value").is_err());
        assert!(parse_game_args("--server @private-value").is_err());
        assert!(parse_game_args("--server localhost --port 25565 --demo ''").is_ok());
    }

    #[test]
    fn game_option_abbreviations_and_single_hyphen_clusters_cannot_bypass_protection() {
        // jopt-simple accepts unique long-name prefixes and also resolves long
        // names after one hyphen. Guard even prefixes that are ambiguous today:
        // the set of options differs between Minecraft versions and loaders.
        for input in [
            "--gameD=private-value",
            "--accessT private-value",
            "-gameDir=private-value",
            "-accessT private-value",
            "--asse=private-value",
            "-UUID=private-value",
            "--userN private-value",
            "--clientI=private-value",
            "--ver=private-value",
            "--s=private-value",
            "-x=private-value",
            "--game'D'=private-value",
            "-gprivate-value",
            "-dgprivate-value",
            "-server",
            "-height=720",
            "-W",
            "-WgameDir=private-value",
        ] {
            let mut args = vec!["original".into()];
            let error = LaunchOptions {
                game_args: input.into(),
                ..Default::default()
            }
            .apply_game(&mut args)
            .unwrap_err();
            assert_eq!(args, ["original"]);
            assert!(!error.contains("private-value"));
        }
        assert_eq!(
            parse_game_args("--server localhost --port 25565 --demo --width=900 --height 600 --usernameSuffix=literal gameDir -12 -0.5 -1e3 - --").unwrap(),
            ["--server", "localhost", "--port", "25565", "--demo", "--width=900", "--height", "600", "--usernameSuffix=literal", "gameDir", "-12", "-0.5", "-1e3", "-", "--"]
        );
    }

    #[test]
    fn managed_window_size_wins_over_appended_game_arguments() {
        let mut args = vec!["--version-option".into()];
        LaunchOptions {
            window: WindowSize::Custom {
                width: 1280,
                height: 720,
            },
            game_args: "--server localhost --width 900 --height=600 --demo ''".into(),
            ..Default::default()
        }
        .apply_game(&mut args)
        .unwrap();
        assert_eq!(
            args,
            [
                "--version-option",
                "--server",
                "localhost",
                "--demo",
                "",
                "--width",
                "1280",
                "--height",
                "720"
            ]
        );
    }

    #[test]
    fn managed_window_size_removes_parser_aliases_without_consuming_other_options() {
        let mut args = [
            "-width=854",
            "--height",
            "480",
            "-w",
            "999",
            "--h=555",
            "--w",
            "-demo",
            "--server",
            "localhost",
            "--height",
            "-12",
        ]
        .map(String::from)
        .to_vec();
        LaunchOptions {
            window: WindowSize::Custom {
                width: 1280,
                height: 720,
            },
            game_args: "--wid 900 --heigh=600 --port 25565".into(),
            ..Default::default()
        }
        .apply_game(&mut args)
        .unwrap();
        assert_eq!(
            args,
            [
                "-demo",
                "--server",
                "localhost",
                "--port",
                "25565",
                "--width",
                "1280",
                "--height",
                "720"
            ]
        );
    }

    #[test]
    fn instance_arguments_inherit_clear_and_publish_atomically() {
        let mut options = LaunchOptions {
            custom_info: "global info".into(),
            jvm_args: "-Dglobal=true".into(),
            game_args: "--demo".into(),
            ..Default::default()
        };
        let original = options.clone();
        LaunchArgumentOverrides::default()
            .apply_to(&mut options)
            .unwrap();
        assert_eq!(options, original);
        let overrides = LaunchArgumentOverrides {
            custom_info: Some(String::new()),
            jvm_args: Some(String::new()),
            game_args: None,
        };
        overrides.validate().unwrap();
        overrides.apply_to(&mut options).unwrap();
        assert!(options.custom_info.is_empty());
        assert!(options.jvm_args.is_empty());
        assert_eq!(options.game_args, "--demo");
        let unchanged = options.clone();
        let invalid = LaunchArgumentOverrides {
            custom_info: Some("would change".into()),
            game_args: Some("--accessToken=private-value".into()),
            ..Default::default()
        };
        assert!(invalid.validate().is_err());
        assert!(invalid.apply_to(&mut options).is_err());
        assert_eq!(options, unchanged);
        assert_eq!(
            serde_json::from_str::<LaunchArgumentOverrides>("{}").unwrap(),
            LaunchArgumentOverrides::default()
        );
        assert_eq!(
            serde_json::from_str::<LaunchArgumentOverrides>(r#"{"jvm_args":""}"#)
                .unwrap()
                .jvm_args,
            Some(String::new())
        );
        assert!(serde_json::from_str::<LaunchArgumentOverrides>(r#"{"unknown":"value"}"#).is_err());
    }

    #[test]
    fn custom_information_counts_unicode_characters_and_rejects_controls() {
        assert!(validate_custom_info(&"🦀".repeat(MAX_CUSTOM_INFO_CHARS)).is_ok());
        assert!(validate_custom_info(&"🦀".repeat(MAX_CUSTOM_INFO_CHARS + 1)).is_err());
        for info in [
            "private-value\n",
            "private-value\t",
            "private-value\0",
            "private-value\u{0085}",
        ] {
            let error = validate_custom_info(info).unwrap_err();
            assert!(!error.contains("private-value"));
        }
        assert!(validate_custom_info("${literal} \"quotes\" $(literal) *").is_ok());
    }
}
