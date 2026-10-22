//! Product-wide metadata and enablement rules. The historical workspace remains
//! the persistence carrier; neither reads nor these rules require Code projects.
use serde_json::Value;
use std::collections::HashSet;
pub const MANIFESTS: &str = include_str!("../../../../../code/plugins.json");
pub fn manifests() -> Value {
    serde_json::from_str(MANIFESTS).expect("bundled plugin manifests")
}
pub fn validate_disabled(disabled_plugins: &[String]) -> Result<(), String> {
    let list = manifests();
    let list = list.as_array().unwrap();
    let mut disabled = HashSet::new();
    for id in disabled_plugins {
        let plugin = list
            .iter()
            .find(|p| p["id"] == *id)
            .ok_or("Unknown stored product plugin")?;
        if plugin["required"] == true || !disabled.insert(id) {
            return Err("Invalid stored plugin state".into());
        }
    }
    for p in list {
        if !disabled_plugins.contains(&p["id"].as_str().unwrap().to_owned())
            && p["dependencies"]
                .as_array()
                .unwrap()
                .iter()
                .any(|id| disabled_plugins.contains(&id.as_str().unwrap().to_owned()))
        {
            return Err("Stored plugin dependency is disabled; retained for recovery".into());
        }
    }
    Ok(())
}
pub fn change(disabled: &mut Vec<String>, args: &Value, active_agent: bool) -> Result<(), String> {
    let key = args["id"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 120)
        .ok_or("Choose a plugin")?;
    let enable = args["enabled"].as_bool().ok_or("Choose a plugin state")?;
    let list = manifests();
    let list = list.as_array().unwrap();
    let plugin = list
        .iter()
        .find(|p| p["id"] == key)
        .ok_or("Unknown bundled plugin")?;
    if !enable && plugin["required"] == true {
        return Err("This plugin is required by RH Code".into());
    }
    if !enable && key == "rh.agent" && active_agent {
        return Err("Stop active Agent tasks before disabling this plugin".into());
    }
    let mut next = disabled.clone();
    next.retain(|v| v != key);
    if !enable {
        next.push(key.to_owned());
    }
    if !enable
        && list.iter().any(|p| {
            !disabled.contains(&p["id"].as_str().unwrap().to_owned())
                && p["dependencies"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|id| id == key)
        })
    {
        return Err("Disable dependent plugins first".into());
    }
    if enable
        && plugin["dependencies"]
            .as_array()
            .unwrap()
            .iter()
            .any(|id| disabled.contains(&id.as_str().unwrap().to_owned()))
    {
        return Err("Enable this plugin's dependencies first".into());
    }
    validate_disabled(&next)?;
    *disabled = next;
    Ok(())
}
