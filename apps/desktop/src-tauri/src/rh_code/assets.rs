//! Native validation and Minecraft export for the two optional visual plugins.
//! Documents are small JSON source assets; exports are generated here instead
//! of accepting renderer-supplied arbitrary binary bytes. File publication is
//! delegated to the existing recoverable, fingerprint-bound native journal.
use super::harness;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};

pub(super) struct AssetWrite {
    pub path: String,
    pub bytes: Vec<u8>,
    pub expected: Option<String>,
}
pub(super) struct Prepared {
    pub plugin: &'static str,
    pub files: Vec<AssetWrite>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Pixel {
    format: String,
    version: u8,
    width: u32,
    height: u32,
    palette: Vec<String>,
    pixels: Vec<usize>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Face {
    uv: [f64; 4],
    #[serde(default)]
    rotation: u16,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Rotation {
    origin: [f64; 3],
    axis: String,
    angle: f64,
    #[serde(default)]
    rescale: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Element {
    id: String,
    from: [f64; 3],
    to: [f64; 3],
    faces: BTreeMap<String, Face>,
    rotation: Option<Rotation>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Model {
    format: String,
    version: u8,
    texture: Pixel,
    elements: Vec<Element>,
}
fn bounded(values: &[f64], min: f64, max: f64) -> bool {
    values
        .iter()
        .all(|v| v.is_finite() && *v >= min && *v <= max)
}
impl Pixel {
    fn rgba(&self) -> Result<Vec<u8>, String> {
        if self.format != "pcl-rh-pixel"
            || self.version != 1
            || !(1..=128).contains(&self.width)
            || !(1..=128).contains(&self.height)
            || self.pixels.len() != (self.width * self.height) as usize
            || self.palette.is_empty()
            || self.palette.len() > 256
            || self.palette[0] != "#00000000"
        {
            return Err("Invalid pixel document dimensions, palette or version".into());
        }
        let mut colors = vec![];
        for color in &self.palette {
            if color.len() != 9
                || !color.starts_with('#')
                || !color.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
            {
                return Err("Pixel colors must use #RRGGBBAA".into());
            }
            let mut rgba = [0; 4];
            for (index, channel) in rgba.iter_mut().enumerate() {
                *channel = u8::from_str_radix(&color[1 + index * 2..3 + index * 2], 16)
                    .map_err(|_| "Invalid pixel color")?;
            }
            colors.push(rgba);
        }
        let mut output = Vec::with_capacity(self.pixels.len() * 4);
        for index in &self.pixels {
            output.extend_from_slice(
                colors
                    .get(*index)
                    .ok_or("Pixel palette index is unavailable")?,
            );
        }
        Ok(output)
    }
    fn png(&self) -> Result<Vec<u8>, String> {
        let pixels = self.rgba()?;
        let mut result = vec![];
        {
            let mut encoder = png::Encoder::new(&mut result, self.width, self.height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
            writer
                .write_image_data(&pixels)
                .map_err(|e| e.to_string())?;
        }
        Ok(result)
    }
}
impl Model {
    fn validate(&self) -> Result<(), String> {
        self.texture.rgba()?;
        if self.format != "pcl-rh-model" || self.version != 1 || self.elements.len() > 128 {
            return Err("Invalid cuboid model format or element count".into());
        }
        let mut ids = HashSet::new();
        for element in &self.elements {
            if element.id.is_empty()
                || element.id.len() > 64
                || !ids.insert(&element.id)
                || !element.id.as_bytes()[0].is_ascii_alphanumeric()
                || !element
                    .id
                    .bytes()
                    .all(|v| v.is_ascii_alphanumeric() || v == b'_' || v == b'-')
                || !bounded(&element.from, -16., 32.)
                || !bounded(&element.to, -16., 32.)
                || element.from.iter().zip(&element.to).any(|(a, b)| a > b)
            {
                return Err("Invalid cuboid identity or bounds".into());
            }
            for (name, face) in &element.faces {
                if !["north", "south", "east", "west", "up", "down"].contains(&name.as_str())
                    || !bounded(&face.uv, 0., 16.)
                    || ![0, 90, 180, 270].contains(&face.rotation)
                {
                    return Err("Invalid cuboid face or texture coordinates".into());
                }
            }
            if let Some(rotation) = &element.rotation {
                if !bounded(&rotation.origin, -16., 32.)
                    || !["x", "y", "z"].contains(&rotation.axis.as_str())
                    || ![-45., -22.5, 0., 22.5, 45.].contains(&rotation.angle)
                {
                    return Err("Invalid Minecraft element rotation".into());
                }
            }
        }
        Ok(())
    }
    fn java_model(&self, resource: &str) -> Result<Vec<u8>, String> {
        self.validate()?;
        texture_resource(resource)?;
        let elements: Vec<_> = self
            .elements
            .iter()
            .map(|element| {
                let faces: serde_json::Map<String, Value> = element
                    .faces
                    .iter()
                    .map(|(name, face)| {
                        (
                            name.clone(),
                            json!({"uv":face.uv,"rotation":face.rotation,"texture":"#texture"}),
                        )
                    })
                    .collect();
                let mut value = json!({"from":element.from,"to":element.to,"faces":faces});
                if let Some(rotation) = &element.rotation {
                    value["rotation"] = json!(rotation);
                }
                value
            })
            .collect();
        serde_json::to_vec_pretty(
            &json!({"textures":{"texture":resource,"particle":resource},"elements":elements}),
        )
        .map_err(|e| e.to_string())
    }
}
fn texture_resource(value: &str) -> Result<(), String> {
    let (namespace, path) = value
        .split_once(':')
        .ok_or("Choose a namespace:path texture resource")?;
    let valid =
        |c: u8| c.is_ascii_lowercase() || c.is_ascii_digit() || [b'_', b'.', b'-'].contains(&c);
    if value.len() > 240
        || namespace.is_empty()
        || !namespace.bytes().all(valid)
        || path.ends_with(".png")
        || !path.bytes().all(|c| valid(c) || c == b'/')
        || path
            .split('/')
            .any(|v| v.is_empty() || v == "." || v == "..")
    {
        return Err("Use a Minecraft namespace:path without .png or relative segments".into());
    }
    Ok(())
}
fn path(args: &Value, key: &str, suffix: &str) -> Result<String, String> {
    let value = args[key].as_str().ok_or("Choose an asset output path")?;
    harness::source_path(value)?;
    if !value.ends_with(suffix) {
        return Err(format!("Asset path must end with {suffix}"));
    }
    Ok(value.into())
}
fn expected(args: &Value) -> Result<Option<String>, String> {
    match args.get("sha256") {
        Some(Value::Null) => Ok(None),
        Some(Value::String(v))
            if v.len() == 64
                && v.bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)) =>
        {
            Ok(Some(v.clone()))
        }
        _ => Err("Asset save requires its original fingerprint or null for a new file".into()),
    }
}
pub(super) fn prepare(operation: &str, args: &Value) -> Result<Prepared, String> {
    let content = args["content"]
        .as_str()
        .filter(|v| v.len() <= 1024 * 1024)
        .ok_or("Asset document exceeds 1 MiB or has no JSON content")?;
    let document: Value = serde_json::from_str(content).map_err(|_| "Invalid asset JSON")?;
    let pixel = document["format"] == "pcl-rh-pixel";
    let (plugin, files) = if pixel {
        let doc: Pixel =
            serde_json::from_value(document).map_err(|_| "Unsupported pixel document fields")?;
        doc.rgba()?;
        let file = if operation == "asset_save" {
            AssetWrite {
                path: path(args, "path", ".rhtexture.json")?,
                bytes: content.as_bytes().to_vec(),
                expected: expected(args)?,
            }
        } else {
            if args["kind"] != "pixel" {
                return Err("Pixel export needs pixel kind".into());
            }
            AssetWrite {
                path: path(args, "path", ".png")?,
                bytes: doc.png()?,
                expected: None,
            }
        };
        ("rh.pixel", vec![file])
    } else {
        if document["elements"].as_array().is_some_and(|elements| {
            elements
                .iter()
                .any(|element| element.get("rotation").is_some_and(Value::is_null))
        }) {
            return Err("Omit a model rotation instead of storing null".into());
        }
        let doc: Model =
            serde_json::from_value(document).map_err(|_| "Unsupported model document fields")?;
        doc.validate()?;
        let files = if operation == "asset_save" {
            vec![AssetWrite {
                path: path(args, "path", ".rhmodel.json")?,
                bytes: content.as_bytes().to_vec(),
                expected: expected(args)?,
            }]
        } else {
            if args["kind"] != "model" {
                return Err("Model export needs model kind".into());
            }
            let resource = args["textureResource"]
                .as_str()
                .ok_or("Choose the model texture resource")?;
            vec![
                AssetWrite {
                    path: path(args, "path", ".json")?,
                    bytes: doc.java_model(resource)?,
                    expected: None,
                },
                AssetWrite {
                    path: path(args, "texturePath", ".png")?,
                    bytes: doc.texture.png()?,
                    expected: None,
                },
            ]
        };
        ("rh.model", files)
    };
    Ok(Prepared { plugin, files })
}

#[cfg(test)]
#[path = "assets/tests.rs"]
mod tests;
