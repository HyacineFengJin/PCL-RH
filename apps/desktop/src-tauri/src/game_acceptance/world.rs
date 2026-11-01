//! Disposable Quick Play world input and game-written join evidence.
//!
//! Version and dimension definitions come from the SHA-1-verified official
//! client installed by the production installer. Only this new instance's
//! private saves/options are written. A valid seed is not join evidence: only
//! Minecraft's subsequently created Quick Play JSON and integrated-server log
//! can establish that the real client joined this world.
use super::Result;
use fastnbt::Value as Nbt;
use flate2::{write::GzEncoder, Compression};
use pcl_core::retained_fs::{self, Directory, Leaf, Links, Metadata, Traversal};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Write},
    path::Path,
};

pub(super) const WORLD_NAME: &str = "AcceptanceWorld";
pub(super) const QUICK_LOG: &str = "acceptance-quickplay.json";
const ENTRY_LIMIT: u64 = 64 * 1024;

pub(super) struct World {
    directory: Directory,
    pub evidence: Value,
}

pub(super) struct Joined {
    // Keep the actual generated file open through owned-game shutdown. A new
    // pathname or a later replacement cannot inherit this join observation.
    file: File,
    stamp: Metadata,
    entry: Value,
    sha256: String,
}

fn leaf(name: &str) -> Result<Leaf> {
    Leaf::new(name).map_err(|e| e.to_string())
}

fn official_inputs(root: &Path, id: &str) -> Result<(Value, Value, Value)> {
    let directory = Directory::open_absolute(root)
        .and_then(|d| d.child(&Leaf::new("versions")?, Traversal::SameFilesystem))
        .and_then(|d| d.child(&Leaf::new(id)?, Traversal::SameFilesystem))
        .map_err(|e| e.to_string())?;
    let name = leaf(&format!("{id}.jar"))?;
    let file = directory
        .open_regular(&name, Links::Single)
        .map_err(|e| e.to_string())?;
    let before = retained_fs::metadata(&file).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    let mut entry = |name: &str| -> Result<(Value, String)> {
        let mut source = archive
            .by_name(name)
            .map_err(|e| format!("Official client lacks {name}: {e}"))?;
        if source.size() == 0 || source.size() > ENTRY_LIMIT {
            return Err("Official world metadata exceeds its bounded entry size".into());
        }
        let mut bytes = Vec::new();
        source
            .by_ref()
            .take(ENTRY_LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > ENTRY_LIMIT || bytes.len() as u64 != source.size() {
            return Err("Official world metadata length changed".into());
        }
        let data = serde_json::from_slice(&bytes)
            .map_err(|e| format!("Official world metadata is invalid: {e}"))?;
        Ok((data, format!("{:x}", Sha256::digest(&bytes))))
    };
    let (version, version_hash) = entry("version.json")?;
    let (preset, preset_hash) = entry("data/minecraft/worldgen/world_preset/flat.json")?;
    drop(entry);
    let file = archive.into_inner();
    let after = retained_fs::metadata(&file).map_err(|e| e.to_string())?;
    if after != before {
        return Err("Official client jar changed during world preparation".into());
    }
    directory
        .recheck(&name, &before.identity)
        .map_err(|e| e.to_string())?;
    Ok((
        version,
        preset,
        json!({"client_jar_identity":before.identity,"version_entry_sha256":version_hash,
        "flat_preset_entry_sha256":preset_hash,"flat_preset_entry":"data/minecraft/worldgen/world_preset/flat.json"}),
    ))
}

fn nbt_json(value: &Value, depth: u8) -> Result<Nbt> {
    if depth > 32 {
        return Err("World preset nesting exceeds its conversion bound".into());
    }
    Ok(match value {
        Value::Bool(value) => Nbt::Byte(if *value { 1 } else { 0 }),
        Value::String(value) => Nbt::String(value.clone()),
        Value::Number(value) => {
            if let Some(integer) = value.as_i64() {
                Nbt::Int(
                    i32::try_from(integer)
                        .map_err(|_| "World preset integer exceeds NBT int range")?,
                )
            } else {
                Nbt::Double(value.as_f64().ok_or("World preset number is invalid")?)
            }
        }
        Value::Array(values) => Nbt::List(
            values
                .iter()
                .map(|v| nbt_json(v, depth + 1))
                .collect::<Result<_>>()?,
        ),
        Value::Object(values) => Nbt::Compound(
            values
                .iter()
                .map(|(key, value)| Ok((key.clone(), nbt_json(value, depth + 1)?)))
                .collect::<Result<_>>()?,
        ),
        Value::Null => return Err("World preset null is not an NBT value".into()),
    })
}

fn compound(entries: impl IntoIterator<Item = (&'static str, Nbt)>) -> Nbt {
    Nbt::Compound(
        entries
            .into_iter()
            .map(|(key, value)| (key.into(), value))
            .collect(),
    )
}

fn level_data(version: &Value, preset: &Value) -> Result<(Vec<u8>, i32)> {
    if version["id"] != "1.21.4"
        || version["name"] != "1.21.4"
        || version["stable"] != true
        || preset["dimensions"]["minecraft:overworld"]["generator"]["type"] != "minecraft:flat"
        || !preset["dimensions"]["minecraft:the_nether"].is_object()
        || !preset["dimensions"]["minecraft:the_end"].is_object()
    {
        return Err("Official client version or complete vanilla flat dimensions do not match acceptance scope".into());
    }
    let data_version = version["world_version"]
        .as_i64()
        .and_then(|n| i32::try_from(n).ok())
        .filter(|n| *n > 0)
        .ok_or("Official client has no valid world data version")?;
    let series = version["series_id"]
        .as_str()
        .ok_or("Official client lacks its world version series")?;
    let rules = [
        ("spawnChunkRadius", "0"),
        ("doMobSpawning", "false"),
        ("doPatrolSpawning", "false"),
        ("doTraderSpawning", "false"),
        ("doDaylightCycle", "false"),
        ("doWeatherCycle", "false"),
        ("mobGriefing", "false"),
        ("randomTickSpeed", "0"),
    ]
    .into_iter()
    .map(|(key, value)| (key, Nbt::String(value.into())));
    let data = compound([
        ("DataVersion", Nbt::Int(data_version)),
        ("version", Nbt::Int(19133)),
        (
            "Version",
            compound([
                ("Id", Nbt::Int(data_version)),
                ("Name", Nbt::String("1.21.4".into())),
                ("Series", Nbt::String(series.into())),
                ("Snapshot", Nbt::Byte(0)),
            ]),
        ),
        ("LevelName", Nbt::String(WORLD_NAME.into())),
        ("GameType", Nbt::Int(1)),
        ("hardcore", Nbt::Byte(0)),
        ("allowCommands", Nbt::Byte(1)),
        ("Difficulty", Nbt::Byte(0)),
        ("DifficultyLocked", Nbt::Byte(1)),
        ("initialized", Nbt::Byte(0)),
        ("LastPlayed", Nbt::Long(0)),
        ("Time", Nbt::Long(0)),
        ("DayTime", Nbt::Long(6000)),
        ("SpawnX", Nbt::Int(0)),
        ("SpawnY", Nbt::Int(0)),
        ("SpawnZ", Nbt::Int(0)),
        ("SpawnAngle", Nbt::Float(0.0)),
        ("clearWeatherTime", Nbt::Int(12000)),
        ("rainTime", Nbt::Int(12000)),
        ("thunderTime", Nbt::Int(12000)),
        ("raining", Nbt::Byte(0)),
        ("thundering", Nbt::Byte(0)),
        ("WasModded", Nbt::Byte(0)),
        ("GameRules", compound(rules)),
        (
            "DataPacks",
            compound([
                ("Enabled", Nbt::List(vec![Nbt::String("vanilla".into())])),
                ("Disabled", Nbt::List(Vec::new())),
            ]),
        ),
        (
            "enabled_features",
            Nbt::List(vec![Nbt::String("minecraft:vanilla".into())]),
        ),
        (
            "WorldGenSettings",
            compound([
                ("seed", Nbt::Long(0)),
                ("generate_features", Nbt::Byte(0)),
                ("bonus_chest", Nbt::Byte(0)),
                ("dimensions", nbt_json(&preset["dimensions"], 0)?),
            ]),
        ),
    ]);
    let root: HashMap<String, Nbt> = [("Data".into(), data)].into_iter().collect();
    let raw = fastnbt::to_bytes(&root).map_err(|e| e.to_string())?;
    let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
    gzip.write_all(&raw).map_err(|e| e.to_string())?;
    Ok((gzip.finish().map_err(|e| e.to_string())?, data_version))
}

impl World {
    pub fn prepare(root: &Path, id: &str, game_dir: &Path) -> Result<Self> {
        if game_dir != root.join("versions").join(id) {
            return Err(
                "Quick Play acceptance requires the isolated production game directory".into(),
            );
        }
        let (version, preset, mut evidence) = official_inputs(root, id)?;
        let (level, data_version) = level_data(&version, &preset)?;
        let directory = Directory::open_absolute(game_dir).map_err(|e| e.to_string())?;
        if directory
            .entry(&leaf(QUICK_LOG)?)
            .map_err(|e| e.to_string())?
            .is_some()
        {
            return Err("Fresh acceptance game directory already contains a Quick Play log".into());
        }
        let saves = directory
            .create_private(&leaf("saves")?, Traversal::SameFilesystem)
            .map_err(|e| e.to_string())?;
        let world = saves
            .create_private(&leaf(WORLD_NAME)?, Traversal::SameFilesystem)
            .map_err(|e| e.to_string())?;
        let mut file = world
            .create_file(&leaf("level.dat")?)
            .map_err(|e| e.to_string())?;
        file.write_all(&level)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        let options = format!("version:{data_version}\nrenderDistance:2\nsimulationDistance:4\nmaxFps:30\nenableVsync:false\ngraphicsMode:0\npauseOnLostFocus:false\nonboardAccessibility:false\ntutorialStep:none\nsoundCategory_master:0.0\n");
        let mut file = directory
            .create_file(&leaf("options.txt")?)
            .map_err(|e| e.to_string())?;
        file.write_all(options.as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        evidence["world_name"] = json!(WORLD_NAME);
        evidence["world_data_version"] = json!(data_version);
        evidence["level_dat_sha256"] = json!(format!("{:x}", Sha256::digest(&level)));
        evidence["world_directory_identity"] =
            serde_json::to_value(world.identity().map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        evidence["quickplay_log_preexisting"] = json!(false);
        evidence["game_dir"] = json!(game_dir);
        evidence["spawn_chunk_radius"] = json!(0);
        evidence["render_distance"] = json!(2);
        evidence["simulation_distance"] = json!(4);
        Ok(Self {
            directory,
            evidence,
        })
    }

    pub fn poll_join(&self, earliest_ms: i64) -> Result<Option<Joined>> {
        let name = leaf(QUICK_LOG)?;
        let Some(entry) = self.directory.entry(&name).map_err(|e| e.to_string())? else {
            return Ok(None);
        };
        if entry.len == 0 {
            return Ok(None);
        }
        if entry.len > ENTRY_LIMIT {
            return Err("Game-written Quick Play log exceeds its evidence limit".into());
        }
        let mut file = self
            .directory
            .open_regular(&name, Links::Single)
            .map_err(|e| e.to_string())?;
        let before = retained_fs::metadata(&file).map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(ENTRY_LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        let after = retained_fs::metadata(&file).map_err(|e| e.to_string())?;
        if after != before || before.identity != entry.identity || bytes.len() as u64 != before.len
        {
            return Ok(None); // The asynchronous game writer has not settled.
        }
        self.directory
            .recheck(&name, &before.identity)
            .map_err(|e| e.to_string())?;
        let Ok(payload) = serde_json::from_slice::<Value>(&bytes) else {
            return Ok(None);
        };
        let Some(entry) = matching_join(&payload, earliest_ms) else {
            return Ok(None);
        };
        Ok(Some(Joined {
            file,
            stamp: before,
            entry: entry.clone(),
            sha256: format!("{:x}", Sha256::digest(&bytes)),
        }))
    }

    pub fn finish_join(&self, joined: Joined, log: &Value) -> Result<Value> {
        let stamp = retained_fs::metadata(&joined.file).map_err(|e| e.to_string())?;
        self.directory
            .recheck(&leaf(QUICK_LOG)?, &joined.stamp.identity)
            .map_err(|e| e.to_string())?;
        if stamp != joined.stamp {
            return Err("Game-written Quick Play evidence changed after join observation".into());
        }
        let markers = log["singleplayer_markers"]
            .as_array()
            .ok_or("Game log has no integrated-server evidence")?;
        let has = |name: &str| markers.iter().any(|marker| marker.as_str() == Some(name));
        let passed = has("integrated_server_started") && has("offline_player_logged_in");
        Ok(
            json!({"quickplay_entry":joined.entry,"quickplay_log_identity":joined.stamp.identity,
            "quickplay_log_sha256":joined.sha256,"native_handle_retained_through_shutdown":true,
            "integrated_server_started":has("integrated_server_started"),"offline_player_logged_in":has("offline_player_logged_in"),
            "player_join_chat":has("player_join_chat"),"passed":passed,"interactive_gameplay":"not_run","authenticated":false}),
        )
    }
}

// 1.21.4's official QuickPlayLog encodes QuickPlayEntry.CODEC.list() and uses
// inline type/id/name fields. Older release notes called the ID "identifier";
// accepting that older shape would conceal a version mismatch in this test.
fn matching_join(payload: &Value, earliest_ms: i64) -> Option<&Value> {
    let entries = payload.as_array()?;
    if entries.len() != 1 {
        return None;
    }
    let entry = &entries[0];
    let joined_at = chrono::DateTime::parse_from_rfc3339(entry["lastPlayedTime"].as_str()?)
        .ok()?
        .timestamp_millis();
    if entry["type"] != "singleplayer"
        || entry["id"] != WORLD_NAME
        || entry["name"] != WORLD_NAME
        || entry["gamemode"] != "creative"
        || joined_at < earliest_ms.saturating_sub(5000)
    {
        return None;
    }
    Some(entry)
}

pub(super) fn epoch_ms() -> Result<i64> {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_millis(),
    )
    .map_err(|e| e.to_string())
}

pub(super) fn singleplayer_log_markers(text: &str) -> Vec<&'static str> {
    let mut markers = Vec::new();
    if text.contains("Starting integrated minecraft server version 1.21.4") {
        markers.push("integrated_server_started");
    }
    if text
        .lines()
        .any(|line| line.contains("PCLAcceptance[") && line.contains(" logged in with entity id "))
    {
        markers.push("offline_player_logged_in");
    }
    if text.contains("PCLAcceptance joined the game") {
        markers.push("player_join_chat");
    }
    markers
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn older_identifier_shape_and_wrong_world_or_time_are_not_join_evidence() {
        let current = json!([{"type":"singleplayer","id":WORLD_NAME,"name":WORLD_NAME,"gamemode":"creative","lastPlayedTime":"2026-11-01T00:00:00Z"}]);
        let since = chrono::DateTime::parse_from_rfc3339("2026-11-01T00:00:00Z")
            .unwrap()
            .timestamp_millis();
        assert!(matching_join(&current, since).is_some());
        let mut old = current.clone();
        old[0].as_object_mut().unwrap().remove("id");
        old[0]["identifier"] = json!(WORLD_NAME);
        assert!(matching_join(&old, since).is_none());
        let mut wrong = current.clone();
        wrong[0]["id"] = json!("OtherWorld");
        assert!(matching_join(&wrong, since).is_none());
        assert!(matching_join(&current, since + 6000).is_none());
        assert!(matching_join(&current[0], since).is_none());
    }

    #[test]
    fn preset_numbers_keep_nbt_int_and_boolean_types() {
        let value = nbt_json(
            &json!({"height":1,"features":false,"layers":[{"height":2}]}),
            0,
        )
        .unwrap();
        let Nbt::Compound(map) = value else {
            panic!("expected compound")
        };
        assert!(matches!(map["height"], Nbt::Int(1)));
        assert!(matches!(map["features"], Nbt::Byte(0)));
        assert!(nbt_json(&Value::Null, 0).is_err());
    }

    #[test]
    fn gzip_world_seed_uses_client_version_and_preserves_long_seed() {
        let version = json!({"id":"1.21.4","name":"1.21.4","stable":true,"world_version":12345,"series_id":"main"});
        let preset = json!({"dimensions":{"minecraft:overworld":{"generator":{"type":"minecraft:flat"}},
            "minecraft:the_nether":{},"minecraft:the_end":{}}});
        let (gzip, version) = level_data(&version, &preset).unwrap();
        assert_eq!(version, 12345);
        let mut decoder = flate2::read::GzDecoder::new(gzip.as_slice());
        let mut raw = Vec::new();
        decoder.read_to_end(&mut raw).unwrap();
        let parsed: HashMap<String, Nbt> = fastnbt::from_bytes(&raw).unwrap();
        let Nbt::Compound(data) = &parsed["Data"] else {
            panic!("missing Data compound")
        };
        assert!(matches!(data["DataVersion"], Nbt::Int(12345)));
        assert!(matches!(data["initialized"], Nbt::Byte(0)));
        let Nbt::Compound(generation) = &data["WorldGenSettings"] else {
            panic!("missing generation compound")
        };
        assert!(matches!(generation["seed"], Nbt::Long(0)));
        assert!(!data.contains_key("Player"));
    }
}
