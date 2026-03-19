use std::path::{Path, PathBuf};
use anyhow::Result;
use serde_json::Value;

/// Shared mutable patch state, kept as JSON to allow serialisation without
/// touching the audio thread.  Updated by the network control layer on every
/// SET / UPDATE / PATCH command.
pub struct PatchState {
    pub json:  Value,
    pub dirty: bool,
    pub path:  Option<PathBuf>,
}

impl PatchState {
    pub fn new(json: Value, path: Option<PathBuf>) -> Self {
        Self { json, dirty: false, path }
    }

    /// Update a single node parameter.  `param_path` = "node-key.param".
    /// If no dot, we can't determine the node — ignored.
    pub fn apply_set(&mut self, param_path: &str, value: f32) {
        let Some((node_key, param)) = param_path.split_once('.') else { return };
        self.update_node_param(node_key, param, value.into());
    }

    /// Apply multiple (path, value) pairs from an UPDATE command.
    #[allow(dead_code)]
    pub fn apply_update(&mut self, pairs: &[(String, f32)]) {
        for (path, value) in pairs {
            self.apply_set(path, *value);
        }
    }

    /// Replace the entire patch JSON (from a PATCH command).
    pub fn apply_patch(&mut self, new_json: Value) {
        self.json  = new_json;
        self.dirty = true;
    }

    /// Write to `{path}.tmp` then rename to `path` (atomic on POSIX).
    /// Clears the dirty flag on success.
    pub fn save(&mut self) -> Result<()> {
        let Some(ref path) = self.path else { return Ok(()) };
        save_atomic(&self.json, path)?;
        self.dirty = false;
        Ok(())
    }

    fn update_node_param(&mut self, node_key: &str, param: &str, value: Value) {
        let Some(chains) = self.json["chains"].as_array_mut() else { return };
        for chain in chains {
            let Some(nodes) = chain["nodes"].as_array_mut() else { continue };
            for node in nodes {
                if node["key"].as_str() == Some(node_key) {
                    node[param] = value;
                    self.dirty = true;
                    return;
                }
            }
        }
    }
}

pub fn save_atomic(json: &Value, path: &Path) -> Result<()> {
    let tmp = path.with_extension(
        format!("{}.tmp", path.extension().and_then(|e| e.to_str()).unwrap_or(""))
    );
    let text = serde_json::to_string_pretty(json)?;
    std::fs::write(&tmp, &text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}
