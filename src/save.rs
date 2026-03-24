use std::collections::HashMap;
use std::path::{Path, PathBuf};
use anyhow::Result;
use serde_json::Value;

/// Use `structure` (from config/preset) as the authoritative chain/node layout,
/// but overlay saved param values from `saved` for any node key that still exists.
/// This ensures structural changes in config are always respected while preserving knob positions.
pub fn merge_state_params(structure: &Value, saved: &Value) -> Value {
    // Collect all saved nodes by key into a flat map.
    let mut saved_nodes: HashMap<&str, &Value> = HashMap::new();
    if let Some(chains) = saved["chains"].as_array() {
        for chain in chains {
            if let Some(nodes) = chain["nodes"].as_array() {
                for node in nodes {
                    if let Some(key) = node["key"].as_str() {
                        saved_nodes.insert(key, node);
                    }
                }
            }
        }
    }

    let mut result = structure.clone();
    if let Some(chains) = result["chains"].as_array_mut() {
        for chain in chains {
            if let Some(nodes) = chain["nodes"].as_array_mut() {
                for node in nodes {
                    let key = match node["key"].as_str() {
                        Some(k) => k.to_string(),
                        None    => continue,
                    };
                    if let Some(saved_node) = saved_nodes.get(key.as_str()) {
                        if let (Some(n), Some(s)) = (node.as_object_mut(), saved_node.as_object()) {
                            for (k, v) in s {
                                if k != "key" && k != "type" {
                                    n.insert(k.clone(), v.clone());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    result
}

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

/// Round all JSON float values to 3 decimal places (removes floating-point noise).
pub fn round_floats(v: &mut Value) {
    match v {
        Value::Number(n) if n.is_f64() => {
            if let Some(f) = n.as_f64() {
                let rounded = (f * 1000.0).round() / 1000.0;
                if let Some(new_n) = serde_json::Number::from_f64(rounded) {
                    *n = new_n;
                }
            }
        }
        Value::Number(_) => {}
        Value::Array(arr) => arr.iter_mut().for_each(round_floats),
        Value::Object(obj) => obj.values_mut().for_each(round_floats),
        _ => {}
    }
}

/// Strip looper-specific transient fields (state, loop_secs, play_count, pos_secs)
/// from all looper nodes before saving.  These fields reflect runtime recording state
/// that cannot be restored from JSON (the audio buffer is not persisted).
pub fn strip_looper_transient(chains: &mut Value) {
    const TRANSIENT: &[&str] = &["state", "loop_secs", "play_count", "pos_secs"];
    let Some(arr) = chains.as_array_mut() else { return };
    for chain in arr {
        let Some(nodes) = chain["nodes"].as_array_mut() else { continue };
        for node in nodes {
            if node["type"].as_str() != Some("looper") { continue }
            let Some(obj) = node.as_object_mut() else { continue };
            for &field in TRANSIENT { obj.remove(field); }
        }
    }
}

pub fn save_atomic(json: &Value, path: &Path) -> Result<()> {
    let tmp = path.with_extension(
        format!("{}.tmp", path.extension().and_then(|e| e.to_str()).unwrap_or(""))
    );
    let mut rounded = json.clone();
    round_floats(&mut rounded);
    let text = serde_json::to_string_pretty(&rounded)?;
    std::fs::write(&tmp, &text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}
