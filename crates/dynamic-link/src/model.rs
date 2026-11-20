//! The link target model and the `.craftlink` file format.
//!
//! A `.craftlink` file is a small JSON document stored next to the project that references a
//! comp/sequence in another running Craft app:
//!
//! ```json
//! {
//!   "schema": 1,
//!   "app": "effectcraft",
//!   "project": "C:/Projects/Show.ecproj",
//!   "item": "Hero Comp",
//!   "port": 9877
//! }
//! ```

use serde::{Deserialize, Serialize};

/// Which Craft app the source project lives in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkApp {
    #[default]
    EffectCraft,
    FilmCraft,
    PhotoCraft,
}

impl LinkApp {
    pub fn id(self) -> &'static str {
        match self {
            LinkApp::EffectCraft => "effectcraft",
            LinkApp::FilmCraft => "filmcraft",
            LinkApp::PhotoCraft => "photocraft",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            LinkApp::EffectCraft => "EffectCraft",
            LinkApp::FilmCraft => "FilmCraft",
            LinkApp::PhotoCraft => "PhotoCraft",
        }
    }
}

/// One linkable item inside a source project: a comp (EffectCraft) or sequence (FilmCraft).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkItem {
    pub id: u64,
    pub name: String,
    /// Width x height of the source item's canvas, in pixels.
    pub width: u32,
    pub height: u32,
    pub frame_rate_num: u32,
    pub frame_rate_den: u32,
    pub duration_seconds: f64,
}

/// What a link points at: the app, its project file, and the item within it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkTarget {
    pub app: LinkApp,
    /// Path to the source project file (absolute when possible).
    pub project: String,
    /// Name of the item inside the source project (looked up at open time by name, falling back to id).
    pub item: String,
    /// The control channel port the source app listens on (`--control PORT`).
    pub port: u16,
    /// Last-known source item dimensions (pixels), cached when the source app was reachable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// Last-known frame rate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_rate_num: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_rate_den: Option<u32>,
    /// Last-known duration in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<f64>,
}

impl LinkTarget {
    pub fn new(app: LinkApp, project: String, item: String, port: u16) -> Self {
        Self { app, project, item, port, width: None, height: None, frame_rate_num: None, frame_rate_den: None, duration_seconds: None }
    }
}

/// The on-disk `.craftlink` file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CraftLinkFile {
    pub schema: u32,
    pub target: LinkTarget,
}

const SCHEMA: u32 = 1;

impl CraftLinkFile {
    pub fn new(target: LinkTarget) -> Self {
        Self { schema: SCHEMA, target }
    }

    /// Encode to the `.craftlink` JSON text.
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }

    /// Decode `.craftlink` JSON text, refusing unknown or too-new schemas.
    pub fn from_json(text: &str) -> serde_json::Result<Self> {
        let v: Self = serde_json::from_str(text)?;
        if v.schema > SCHEMA {
            return Err(serde::de::Error::custom(format!(
                "unsupported .craftlink schema {} (this build supports {})",
                v.schema, SCHEMA
            )));
        }
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_json() {
        let file = CraftLinkFile::new(LinkTarget {
            app: LinkApp::EffectCraft,
            project: "C:/Projects/Show.ecproj".into(),
            item: "Hero Comp".into(),
            port: 9877,
        });
        let text = file.to_json().unwrap();
        let back = CraftLinkFile::from_json(&text).unwrap();
        assert_eq!(back, file);
        assert_eq!(back.target.app.id(), "effectcraft");
    }

    #[test]
    fn rejects_too_new_schema() {
        let bad = r#"{"schema":9,"target":{"app":"effectcraft","project":"p.ecproj","item":"c","port":1}}"#;
        assert!(CraftLinkFile::from_json(bad).is_err());
    }
}
