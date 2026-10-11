//! Layout data describes view placement, never plugin execution rights. Unknown
//! view IDs and their project state survive plugin removal/reinstallation. Only
//! portable presets discard project bindings, after validating the full input.
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    path::{Component, Path},
};

const LAYOUT_BYTES: usize = 262_144;
const STATE_BYTES: usize = 65_536;

fn identifier(value: &Value, max: usize) -> Result<&str, String> {
    let value = value.as_str().ok_or("Invalid layout identifier")?;
    if value.is_empty()
        || value.len() > max
        || !value.as_bytes()[0].is_ascii_alphanumeric()
        || !value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
    {
        return Err("Invalid layout identifier".into());
    }
    Ok(value)
}

fn checked_state(value: &Value, depth: usize, nodes: &mut usize) -> Result<(), String> {
    *nodes += 1;
    // The count limits traversal work before serialization measures exact bytes.
    if depth > 8 || *nodes > STATE_BYTES {
        return Err("View state is too deep or large".into());
    }
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if key.is_empty()
                    || key.len() > 120
                    || ["__proto__", "constructor", "prototype"].contains(&key.as_str())
                    || !key
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
                {
                    return Err("Invalid view state key".into());
                }
                checked_state(value, depth + 1, nodes)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                checked_state(value, depth + 1, nodes)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn checked_area_state(state: &Value) -> Result<(), String> {
    let object = state.as_object().ok_or("View state must be a map")?;
    for (view, state) in object {
        identifier(&Value::String(view.clone()), 120)?;
        if !state.is_object() {
            return Err("Each view state must be an object".into());
        }
    }
    checked_state(state, 0, &mut 0)?;
    if serde_json::to_vec(state).map_err(|e| e.to_string())?.len() > STATE_BYTES {
        return Err("Area view state exceeds 64 KiB".into());
    }
    Ok(())
}

fn checked_file(value: &Value) -> Result<(), String> {
    let value = value.as_str().ok_or("Invalid layout file binding")?;
    let path = Path::new(value);
    if value.trim().is_empty()
        || value.len() > 4096
        || value.chars().any(char::is_control)
        || value.contains('\\')
        || value.contains(':')
        || path.is_absolute()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        || value
            .split('/')
            .any(|part| part.is_empty() || part.starts_with('.'))
    {
        return Err("Invalid layout file binding".into());
    }
    Ok(())
}

fn checked_node(
    node: &Value,
    seen: &mut HashSet<String>,
    areas: &mut usize,
    depth: usize,
) -> Result<(), String> {
    let object = node.as_object().ok_or("Invalid workspace area")?;
    if depth > 7 || !seen.insert(identifier(&node["id"], 100)?.to_owned()) {
        return Err("Invalid workspace layout depth or duplicate ID".into());
    }
    let fields: &[&str] = match node["type"].as_str() {
        Some("area") => &["type", "id", "view", "conversation", "file", "state"],
        Some("split") => &["type", "id", "axis", "ratio", "children"],
        _ => return Err("Invalid workspace area type".into()),
    };
    if object.keys().any(|key| !fields.contains(&key.as_str())) {
        return Err("Unknown workspace layout field".into());
    }
    if node["type"] == "area" {
        *areas += 1;
        if *areas > 12 {
            return Err("Workspace layout exceeds 12 areas".into());
        }
        identifier(&node["view"], 120)?;
        if let Some(conversation) = object.get("conversation") {
            identifier(conversation, 100)?;
        }
        if let Some(file) = object.get("file") {
            checked_file(file)?;
        }
        if let Some(state) = object.get("state") {
            checked_area_state(state)?;
        }
    } else {
        if ![Some("row"), Some("column")].contains(&node["axis"].as_str())
            || !node["ratio"]
                .as_f64()
                .is_some_and(|ratio| (0.15..=0.85).contains(&ratio))
        {
            return Err("Invalid workspace split".into());
        }
        let children = node["children"]
            .as_array()
            .filter(|children| children.len() == 2)
            .ok_or("Invalid split children")?;
        for child in children {
            checked_node(child, seen, areas, depth + 1)?;
        }
    }
    Ok(())
}

pub(super) fn checked_layout(node: &Value) -> Result<(), String> {
    // Null denotes an uninitialized/empty project, and is never a split child.
    if !node.is_null() {
        checked_node(node, &mut HashSet::new(), &mut 0, 0)?;
        if serde_json::to_vec(node).map_err(|e| e.to_string())?.len() > LAYOUT_BYTES {
            return Err("Workspace layout exceeds 256 KiB".into());
        }
    }
    Ok(())
}

fn portable_node(node: &Value) -> Value {
    if node["type"] == "area" {
        json!({"id":node["id"],"type":"area","view":node["view"]})
    } else {
        json!({"id":node["id"],"type":"split","axis":node["axis"],"ratio":node["ratio"],
            "children":[portable_node(&node["children"][0]),portable_node(&node["children"][1])]})
    }
}

pub(super) fn portable_layout(node: &Value) -> Result<Value, String> {
    checked_layout(node)?;
    if node.is_null() {
        return Err("Choose a nonempty layout preset".into());
    }
    Ok(portable_node(node))
}
