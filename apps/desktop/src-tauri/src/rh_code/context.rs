//! Durable context belongs to the native project and conversation. Admission
//! validates file references against the frozen text manifest and copies a
//! bounded, contiguous history suffix. Later metadata edits affect future jobs;
//! neither this projection nor its consumers can rewrite the canonical log.
use super::{harness, Message};
use serde_json::{json, Value};
use std::collections::HashSet;

const NOTE_CHARS: usize = 8000;
const NOTE_BYTES: usize = 24000;
const PATHS: usize = 20;
const HISTORY_MESSAGES: usize = 1024;
const HISTORY_BYTES: usize = 1_000_000;
const HOST_REQUEST_BYTES: usize = 24 * 1024 * 1024;

pub(super) fn checked_note(value: &str, name: &str) -> Result<(), String> {
    if value.chars().count() > NOTE_CHARS
        || value.len() > NOTE_BYTES
        || value
            .chars()
            .any(|c| c == '\0' || (c.is_control() && c != '\n' && c != '\t'))
    {
        return Err(format!(
            "{name} exceeds 8000 characters or 24000 UTF-8 bytes, or contains invalid controls"
        ));
    }
    Ok(())
}

pub(super) fn note(args: &Value, key: &str) -> Result<String, String> {
    let value = args[key].as_str().ok_or_else(|| format!("Missing {key}"))?;
    checked_note(value, key)?;
    Ok(value.to_owned())
}

pub(super) fn checked_paths(paths: &[String]) -> Result<(), String> {
    if paths.len() > PATHS {
        return Err("Conversation context supports at most 20 file references".into());
    }
    let mut seen = HashSet::new();
    for path in paths {
        // Use the same visible, canonical relative path contract as capture and
        // proposal application, so excluded files never become context refs.
        harness::source_path(path)?;
        if !seen.insert(path) {
            return Err("Conversation context file references must be unique".into());
        }
    }
    Ok(())
}

pub(super) fn paths(args: &Value) -> Result<Vec<String>, String> {
    let paths: Vec<String> = serde_json::from_value(args["contextPaths"].clone())
        .map_err(|_| "Choose contextPaths as a list of project file paths")?;
    checked_paths(&paths)?;
    Ok(paths)
}

pub(super) fn require_available(paths: &[String], capture: &Value) -> Result<(), String> {
    let manifest = capture["sourceManifest"]
        .as_array()
        .ok_or("Invalid native Harness text manifest")?;
    let available: HashSet<&str> = manifest
        .iter()
        .filter_map(|entry| entry["path"].as_str())
        .collect();
    for path in paths {
        if !available.contains(path.as_str()) {
            return Err(format!("Context file reference is unavailable in the captured text snapshot: {path}. Choose an existing visible UTF-8 text file."));
        }
    }
    Ok(())
}

fn history_envelope(start: usize, total: usize, messages: Value) -> Value {
    json!({"schemaVersion":1,"messages":messages,"startIndex":start,"totalMessages":total,"truncated":start != 0})
}

pub(super) fn history(messages: &[Message]) -> Result<Value, String> {
    let value = history_with_budget(messages, HISTORY_BYTES)?;
    if !messages.is_empty() && value["messages"].as_array().unwrap().is_empty() {
        return Err(
            "Newest conversation message exceeds the 1000000-byte captured history budget".into(),
        );
    }
    Ok(value)
}

fn history_with_budget(messages: &[Message], budget: usize) -> Result<Value, String> {
    let total = messages.len();
    let mut start = total;
    let mut encoded_messages = 0usize;
    while start != 0 && total - start < HISTORY_MESSAGES {
        let candidate = start - 1;
        let size = serde_json::to_vec(&messages[candidate])
            .map_err(|e| e.to_string())?
            .len();
        let count = total - candidate;
        let envelope = serde_json::to_vec(&history_envelope(candidate, total, json!([])))
            .map_err(|e| e.to_string())?
            .len();
        // Empty brackets are already in the envelope. Nonempty arrays add only
        // complete messages and their separators; UTF-8 and JSON escaping both
        // count toward the transport budget.
        if envelope + encoded_messages + size + count - 1 > budget {
            break;
        }
        encoded_messages += size;
        start = candidate;
    }
    let suffix = serde_json::to_value(&messages[start..]).map_err(|e| e.to_string())?;
    Ok(history_envelope(start, total, suffix))
}

fn wire_size(context: &Value) -> Result<usize, String> {
    let payload = serde_json::to_vec(context)
        .map_err(|e| e.to_string())?
        .len();
    let envelope = br#"{"operation":"rh_code_run","args":{}}"#.len() - 2;
    Ok(payload + envelope)
}

pub(super) fn check_host_budget(context: &Value) -> Result<(), String> {
    if wire_size(context)? > HOST_REQUEST_BYTES {
        return Err(
            "Captured Harness context exceeds the 24 MiB native host request budget".into(),
        );
    }
    Ok(())
}

pub(super) fn fit_host_budget(context: &mut Value, messages: &[Message]) -> Result<(), String> {
    // Match Session::call's wire envelope. Sixteen MiB of base64 source needs
    // about 22.4 MB; ordinary maximum-length path metadata plus 1 MB history fits
    // 24 MiB. Escaped filename bytes can use more room, so optional history gives
    // way to required source/notes. It still remains a complete contiguous suffix,
    // possibly empty. Required metadata alone must fit the host's exact cap.
    let bytes = wire_size(context)?;
    if bytes <= HOST_REQUEST_BYTES {
        return Ok(());
    }
    let history_bytes = serde_json::to_vec(&context["conversationHistory"])
        .map_err(|e| e.to_string())?
        .len();
    let available = HOST_REQUEST_BYTES.saturating_sub(bytes - history_bytes);
    context["conversationHistory"] = history_with_budget(messages, available.min(HISTORY_BYTES))?;
    check_host_budget(context)
}
