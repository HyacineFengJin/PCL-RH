//! Libraries that modern Forge locates itself rather than adding to the JVM
//! classpath. Keep this list shared by launch inspection and repair enumeration:
//! a successful classpath check alone does not establish a usable Forge install.
use crate::{game_argument, identifier, Result};
use serde_json::Value;

/// The BootstrapLauncher Forge client protocol supplies these exact coordinate
/// parts to CommonClientLaunchHandler/ForgeClientLaunchHandler. Older Forge,
/// Fabric and the newer NeoForge protocol use different layouts and are not
/// inferred from a similar display name.
pub(crate) fn required_paths(data: &Value) -> Result<Vec<String>> {
    if data["mainClass"] != "cpw.mods.bootstraplauncher.BootstrapLauncher"
        || game_argument(data, "--launchTarget") != Some("forgeclient")
        || game_argument(data, "--fml.forgeGroup") != Some("net.minecraftforge")
    {
        return Ok(Vec::new());
    }
    let Some(mc) = game_argument(data, "--fml.mcVersion") else {
        return Ok(Vec::new());
    };
    let Some(forge) = game_argument(data, "--fml.forgeVersion") else {
        return Ok(Vec::new());
    };
    let Some(mcp) = game_argument(data, "--fml.mcpVersion") else {
        return Ok(Vec::new());
    };
    for part in [mc, forge, mcp] {
        identifier(part)?;
        if part.len() > 160
            || !part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
        {
            return Err("Forge 运行库版本标识无效".into());
        }
    }
    let version = format!("{mc}-{forge}");
    let mut paths: Vec<_> = [
        "fmlcore",
        "javafmllanguage",
        "lowcodelanguage",
        "mclanguage",
    ]
    .into_iter()
    .map(|name| format!("libraries/net/minecraftforge/{name}/{version}/{name}-{version}.jar"))
    .collect();
    for classifier in ["client", "universal"] {
        paths.push(format!(
            "libraries/net/minecraftforge/forge/{version}/forge-{version}-{classifier}.jar"
        ));
    }
    for classifier in ["srg", "extra"] {
        paths.push(format!(
            "libraries/net/minecraft/client/{mc}-{mcp}/client-{mc}-{mcp}-{classifier}.jar"
        ));
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    pub(super) fn profile() -> Value {
        json!({"id":"sample","mainClass":"cpw.mods.bootstraplauncher.BootstrapLauncher",
            "libraries":[],"arguments":{"game":["--launchTarget","forgeclient",
            "--fml.forgeGroup","net.minecraftforge","--fml.mcVersion","1.20.1",
            "--fml.forgeVersion","47.4.0","--fml.mcpVersion","20230612.114412"]}})
    }

    #[test]
    fn implicit_forge_files_are_not_confused_with_classpath_libraries() {
        let paths = required_paths(&profile()).unwrap();
        assert_eq!(paths.len(), 8);
        assert!(paths
            .iter()
            .any(|p| p.ends_with("fmlcore-1.20.1-47.4.0.jar")));
        assert!(paths
            .iter()
            .any(|p| p.ends_with("client-1.20.1-20230612.114412-extra.jar")));
    }

    #[test]
    fn unrelated_protocols_are_not_guessed_and_coordinates_cannot_escape() {
        let mut data = profile();
        data["arguments"]["game"][3] = json!("net.neoforged");
        assert!(required_paths(&data).unwrap().is_empty());
        data = profile();
        data["arguments"]["game"][7] = json!("../47");
        assert!(required_paths(&data).is_err());
        data = profile();
        data["mainClass"] = json!("net.minecraft.launchwrapper.Launch");
        assert!(required_paths(&data).unwrap().is_empty());
    }

    #[test]
    fn inspection_catches_implicit_files_without_adding_them_to_the_classpath() {
        let root = tempfile::tempdir().unwrap();
        let data = profile();
        std::fs::create_dir_all(root.path().join("versions/sample")).unwrap();
        std::fs::write(root.path().join("versions/sample/sample.jar"), b"client").unwrap();
        let context = crate::rules::RuleContext::current().unwrap();
        let first = crate::dependencies::resolve(root.path(), "sample", &data, &context).unwrap();
        assert_eq!(first.missing.total, 8);
        assert_eq!(first.classpath.len(), 1);
        for path in required_paths(&data).unwrap() {
            let file = root.path().join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, b"runtime").unwrap();
        }
        let complete =
            crate::dependencies::resolve(root.path(), "sample", &data, &context).unwrap();
        assert_eq!(complete.missing.total, 0);
        assert_eq!(complete.classpath.len(), 1);
        std::fs::remove_file(
            root.path().join(
                "libraries/net/minecraftforge/fmlcore/1.20.1-47.4.0/fmlcore-1.20.1-47.4.0.jar",
            ),
        )
        .unwrap();
        let damaged = crate::dependencies::resolve(root.path(), "sample", &data, &context).unwrap();
        assert_eq!(damaged.missing.total, 1);
        assert!(damaged.missing.paths[0].ends_with("fmlcore-1.20.1-47.4.0.jar"));
    }
}
