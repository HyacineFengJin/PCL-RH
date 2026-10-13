use super::*;
use crate::rh_code::{hash, id, Store};
use std::{fs, path::PathBuf};

struct Fixture {
    root: PathBuf,
    folder: PathBuf,
    store: Store,
    project: String,
}
impl Fixture {
    fn new() -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../work/visual-assets-2026-10-13/native")
            .join(id());
        let folder = root.join("project");
        fs::create_dir_all(&folder).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let folder = fs::canonicalize(folder).unwrap();
        let store = Store::new(&root);
        let revision = store.call("bootstrap", json!({})).unwrap()["revision"].clone();
        let state = store
            .call(
                "project_create",
                json!({"name":"Visual fixture","folder":folder,"expectedRevision":revision}),
            )
            .unwrap();
        let project = state["projects"][0]["id"].as_str().unwrap().into();
        Self {
            root,
            folder,
            store,
            project,
        }
    }
    fn write(&self, op: &str, mut args: Value) -> Result<Value, String> {
        args["projectId"] = json!(self.project);
        args["expectedRevision"] = self.store.call("bootstrap", json!({}))?["revision"].clone();
        self.store.call(op, args)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn pixel() -> Value {
    json!({"format":"pcl-rh-pixel","version":1,"width":2,"height":1,
        "palette":["#00000000","#F08040FF"],"pixels":[1,0]})
}
fn model() -> Value {
    json!({"format":"pcl-rh-model","version":1,"texture":pixel(),"elements":[
        {"id":"cube","from":[0,0,0],"to":[16,8,16],"faces":{"north":{"uv":[0,0,16,16],"rotation":90}}}
    ]})
}
#[test]
fn asset_document_creation_save_conflict_and_binary_export() {
    let f = Fixture::new();
    let original = pixel().to_string();
    f.write(
        "asset_save",
        json!({"path":"art/check.rhtexture.json","content":original,"sha256":null}),
    )
    .unwrap();
    let path = f.folder.join("art/check.rhtexture.json");
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    assert!(f
        .write(
            "asset_save",
            json!({"path":"art/check.rhtexture.json","content":original,"sha256":null})
        )
        .is_err());
    let mut changed = pixel();
    changed["pixels"] = json!([0, 1]);
    f.write("asset_save",json!({"path":"art/check.rhtexture.json","content":changed.to_string(),"sha256":hash(original.as_bytes())})).unwrap();
    assert!(f.write("asset_save",json!({"path":"art/check.rhtexture.json","content":original,"sha256":hash(original.as_bytes())})).is_err());
    assert_eq!(fs::read_to_string(path).unwrap(), changed.to_string());
    f.write(
        "asset_export",
        json!({"kind":"pixel","path":"assets/test/textures/block/check.png","content":original}),
    )
    .unwrap();
    let png = fs::read(f.folder.join("assets/test/textures/block/check.png")).unwrap();
    let mut decoder = png::Decoder::new(std::io::Cursor::new(png))
        .read_info()
        .unwrap();
    let mut output = vec![0; decoder.output_buffer_size()];
    let info = decoder.next_frame(&mut output).unwrap();
    assert_eq!((info.width, info.height), (2, 1));
    assert_eq!(
        &output[..info.buffer_size()],
        &[240, 128, 64, 255, 0, 0, 0, 0]
    );
}
#[test]
fn model_exports_pair_and_conflicts_publish_neither_output() {
    let f = Fixture::new();
    let args = json!({"kind":"model","path":"assets/test/models/block/check.json",
        "texturePath":"assets/test/textures/block/check.png","textureResource":"test:block/check","content":model().to_string()});
    let texture = f.folder.join("assets/test/textures/block/check.png");
    fs::create_dir_all(texture.parent().unwrap()).unwrap();
    fs::write(&texture, b"existing content").unwrap();
    assert!(f.write("asset_export", args.clone()).is_err());
    assert!(!f
        .folder
        .join("assets/test/models/block/check.json")
        .exists());
    assert_eq!(fs::read(&texture).unwrap(), b"existing content");
    fs::remove_file(&texture).unwrap();
    f.write("asset_export", args.clone()).unwrap();
    let output: Value = serde_json::from_slice(
        &fs::read(f.folder.join("assets/test/models/block/check.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(output["textures"]["texture"], "test:block/check");
    assert_eq!(output["elements"][0]["to"][1].as_f64(), Some(8.));
    assert_eq!(output["elements"][0]["faces"]["north"]["rotation"], 90);
    assert!(fs::read(&texture).unwrap().starts_with(b"\x89PNG"));
    assert!(f.write("asset_export", args).is_err());
    // Both binary and text entries have complete durable journals understood by
    // the same recovery path used before future captures and writes.
    let boot = f.store.call("bootstrap", json!({})).unwrap();
    let project: crate::rh_code::Project =
        serde_json::from_value(boot["projects"][0].clone()).unwrap();
    let recovered = harness::recover(&f.store.root, &project).unwrap();
    assert_eq!(recovered["pending"], 0);
}
#[test]
fn asset_plugin_validation_and_stale_revision_keep_files() {
    let f = Fixture::new();
    let stale = f.store.call("bootstrap", json!({})).unwrap()["revision"].clone();
    f.write("plugin_set", json!({"id":"rh.pixel","enabled":false}))
        .unwrap();
    let args = json!({"path":"paint.rhtexture.json","content":pixel().to_string(),"sha256":null});
    assert!(f
        .write("asset_save", args.clone())
        .unwrap_err()
        .contains("Plugin disabled"));
    f.write("plugin_set", json!({"id":"rh.pixel","enabled":true}))
        .unwrap();
    f.write(
        "project_instructions",
        json!({"instructions":"Changed after the captured revision"}),
    )
    .unwrap();
    let mut stale_args = args.clone();
    stale_args["projectId"] = json!(f.project);
    stale_args["expectedRevision"] = stale;
    assert!(f.store.call("asset_save", stale_args).is_err());
    let mut invalid = pixel();
    invalid["width"] = json!(129);
    assert!(f
        .write(
            "asset_save",
            json!({"path":"paint.rhtexture.json","content":invalid.to_string(),"sha256":null})
        )
        .is_err());
    assert!(f
        .write(
            "asset_save",
            json!({"path":"../paint.rhtexture.json","content":pixel().to_string(),"sha256":null})
        )
        .is_err());
    let mut null_rotation = model();
    null_rotation["elements"][0]["rotation"] = Value::Null;
    assert!(f
        .write(
            "asset_save",
            json!({"path":"paint.rhmodel.json","content":null_rotation.to_string(),"sha256":null})
        )
        .is_err());
    assert!(fs::read_dir(&f.folder).unwrap().next().is_none());
}
#[test]
fn asset_linked_parents_and_hardlinks_cannot_redirect_saves() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let outside = f.root.join("outside");
    fs::create_dir(&outside).unwrap();
    symlink(&outside, f.folder.join("linked")).unwrap();
    let content = pixel().to_string();
    assert!(f
        .write(
            "asset_save",
            json!({"path":"linked/paint.rhtexture.json","content":content,"sha256":null})
        )
        .is_err());
    fs::write(outside.join("original"), &content).unwrap();
    fs::hard_link(
        outside.join("original"),
        f.folder.join("hard.rhtexture.json"),
    )
    .unwrap();
    assert!(f.write("asset_save",json!({"path":"hard.rhtexture.json","content":content,"sha256":hash(content.as_bytes())})).is_err());
    assert_eq!(
        fs::read_to_string(outside.join("original")).unwrap(),
        content
    );
    assert!(!outside.join("paint.rhtexture.json").exists());
}

#[test]
fn shared_js_native_fixtures_preserve_rgba_and_java_model_semantics() {
    fn semantic_equal(actual: &Value, expected: &Value, path: &str) {
        match (actual, expected) {
            (Value::Number(left), Value::Number(right)) => {
                // Native coordinates use f64; JS JSON emits whole values as
                // integers. Their values must match without requiring identical
                // serialized numeric spelling or JSON object insertion order.
                assert_eq!(left.as_f64(), right.as_f64(), "number mismatch at {path}");
            }
            (Value::Array(left), Value::Array(right)) => {
                assert_eq!(left.len(), right.len(), "array length mismatch at {path}");
                for (index, (left, right)) in left.iter().zip(right).enumerate() {
                    semantic_equal(left, right, &format!("{path}[{index}]"));
                }
            }
            (Value::Object(left), Value::Object(right)) => {
                assert_eq!(left.len(), right.len(), "object field mismatch at {path}");
                for (key, expected) in right {
                    let actual = left
                        .get(key)
                        .unwrap_or_else(|| panic!("missing JSON field {path}.{key}"));
                    semantic_equal(actual, expected, &format!("{path}.{key}"));
                }
            }
            _ => assert_eq!(actual, expected, "JSON value mismatch at {path}"),
        }
    }

    fn expected_rgba(fixture: &Value, document: &Pixel) -> Vec<u8> {
        let rows = fixture["rgbaRows"].as_array().unwrap();
        assert_eq!(rows.len(), document.height as usize);
        let mut bytes = Vec::with_capacity((document.width * document.height * 4) as usize);
        for row in rows {
            let row = row.as_str().unwrap();
            assert_eq!(row.len(), document.width as usize * 8);
            assert!(row.bytes().all(|value| value.is_ascii_hexdigit()));
            for offset in (0..row.len()).step_by(2) {
                bytes.push(u8::from_str_radix(&row[offset..offset + 2], 16).unwrap());
            }
        }
        bytes
    }

    let fixtures: Value = serde_json::from_str(include_str!("fixtures.json")).unwrap();
    assert_eq!(fixtures["schemaVersion"], 1);
    let textures = fixtures["textures"].as_array().unwrap();
    let models = fixtures["models"].as_array().unwrap();
    assert_eq!(textures.len(), 2);
    assert_eq!(models.len(), 2);

    for fixture in textures {
        let document: Pixel = serde_json::from_value(fixture["document"].clone()).unwrap();
        let expected = expected_rgba(fixture, &document);
        assert_eq!(document.rgba().unwrap(), expected, "{}", fixture["name"]);

        let png = document.png().unwrap();
        let mut reader = png::Decoder::new(std::io::Cursor::new(png))
            .read_info()
            .unwrap();
        let mut decoded = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut decoded).unwrap();
        assert_eq!((info.width, info.height), (document.width, document.height));
        assert_eq!(info.color_type, png::ColorType::Rgba);
        assert_eq!(info.bit_depth, png::BitDepth::Eight);
        assert_eq!(&decoded[..info.buffer_size()], expected.as_slice());
    }

    for fixture in models {
        let texture = textures
            .iter()
            .find(|texture| texture["name"] == fixture["texture"])
            .unwrap();
        let document: Model = serde_json::from_value(json!({
            "format": "pcl-rh-model",
            "version": 1,
            "texture": texture["document"],
            "elements": fixture["elements"]
        }))
        .unwrap();
        document.validate().unwrap();
        assert_eq!(
            document.texture.rgba().unwrap(),
            expected_rgba(texture, &document.texture)
        );
        let exported: Value = serde_json::from_slice(
            &document
                .java_model(fixtures["textureResource"].as_str().unwrap())
                .unwrap(),
        )
        .unwrap();
        semantic_equal(
            &exported,
            &fixture["javaModel"],
            fixture["name"].as_str().unwrap(),
        );
    }
}
