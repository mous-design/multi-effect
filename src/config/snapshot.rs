use std::collections::HashMap;
use std::path::Path;
use anyhow::{Result, bail};
use serde_json::Value;
use serde::{Deserialize, Serialize};
use super::preset::PresetDefs;
use super::persist_fs::{persist, load};
use super::preset::PresetDef;
use super::Config;
use crate::control::mapping::ControllerDef;
use crate::engine::device::{MetaTarget, ParamInfo, ParamValue};

#[derive(Debug, Copy, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum SnapshotState {
    #[default]
    Clean,
    CleanPersisted,
    Dirty,
    DirtyPersisted,
    Comparing,
    ComparingPersisted,
}
use SnapshotState::*;

impl SnapshotState {
    /// Simplified name for external consumers (serial/net/UI).
    /// Strips the Persisted suffix — controllers don't care about persistence.
    pub fn label(&self) -> &'static str {
        match self {
            Clean | CleanPersisted => "Clean",
            Dirty | DirtyPersisted => "Dirty",
            Comparing | ComparingPersisted => "Comparing",
        }
    }
}

/// Read-only snapshot published via watch channel after every mutation.
///
/// Serialization shape is full (including `stash`) — used for persistence to disk.
/// For wire output (e.g. `SNAPSHOT` line over WS), use [`Self::to_view`] which
/// returns a borrowed view without the internal `stash` field.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigSnapshot {
    pub state:    SnapshotState,
    pub preset:      PresetDef,
    pub stash: Option<PresetDef>,
    pub preset_indices:  Vec<u8>,
}

/// Wire projection of a `NodeDef`. Same JSON shape as on-disk `NodeDef`
/// (live values flatten to the top, `overrides` stays a sibling), but adds
/// the master-computed resolved `params_info` array. The UI uses this to
/// render whatever knobs the effect declares — without hardcoded knowledge
/// of effect types.
#[derive(Clone, Debug, Serialize)]
pub struct NodeView {
    pub key: String,
    #[serde(rename = "type")]
    pub device_type: String,
    /// Per-instance metadata overrides currently applied (already baked into
    /// `params_info`; kept here so the UI can show "this knob has an Instance
    /// edit" affordances).
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub overrides: HashMap<MetaTarget, ParamValue>,
    /// Resolved per-instance ParamInfo array: canonical → Type overrides →
    /// Instance overrides applied.
    pub params_info: Vec<ParamInfo>,
    /// Live param values (the flattened scalars: `wet`, `room_size`, …).
    #[serde(flatten)]
    pub params: serde_json::Map<String, Value>,
}

/// Wire projection of a `ChainDef`.
#[derive(Clone, Debug, Serialize)]
pub struct ChainView {
    pub input:  [u8; 2],
    pub output: [u8; 2],
    pub nodes:  Vec<NodeView>,
}

/// Wire projection of a `PresetDef`.
#[derive(Clone, Debug, Serialize)]
pub struct PresetView {
    pub index:       u8,
    pub chains:      Vec<ChainView>,
    pub controllers: Vec<ControllerDef>,
}

/// Wire projection of [`ConfigSnapshot`]: everything a client needs to render,
/// without the internal `stash` field. Owned (so it can cross task boundaries
/// via oneshot / broadcast channels). Master builds it via
/// [`crate::config::master::ConfigMaster::build_snapshot_view`].
#[derive(Clone, Debug, Serialize)]
pub struct SnapshotView {
    pub state:          SnapshotState,
    pub preset:         PresetView,
    pub preset_indices: Vec<u8>,
}

impl ConfigSnapshot {
    pub fn restore_or_build(cfg: &Config) -> Result<Self> {
        Ok(Self::restore_state(&cfg.state_save_path)?.unwrap_or_else(|| Self::build(cfg)))
    }

    fn build(cfg: &Config) -> Self {
        let preset_indices = cfg.presets.indices();
        if let Some(preset) = cfg.presets.active_entry() {
            Self { state: Clean, preset: preset.clone(), stash: None, preset_indices }
        } else {
            Self { state: Dirty, preset: PresetDef::default(), stash: None, preset_indices}
        }
    }
    
    pub fn load_preset(&mut self, preset: PresetDef, state: SnapshotState ) {
        self.stash = None;
        self.preset = preset;
        self.state = state;
    }

    /// Update a single node parameter.  `param_path` = "node-key.param". Return if changed
    pub fn apply_set(&mut self, param_path: &str, value: f32) -> Result<bool> {
        let Some((node_key, param)) = param_path.split_once('.') else {
            bail!("invalid param path '{param_path}': missing '.'");
        };
        for chain in self.preset.chains.iter_mut() {
            for node in chain.nodes.iter_mut() {
                if node.key == node_key {
                    let new = Value::from(value);
                    if node.params.get(param) != Some(&new) {
                        node.params[param] = new;
                        // If comparing, we can delete the stash
                        if matches!(self.state, Comparing | ComparingPersisted) {
                            self.stash = None;
                        }
                        return Ok(true);
                    } else {
                        return Ok(false);
                    }
                }
            }
        }
        bail!("No node found with key {node_key}")
    }

    pub fn set_state(&mut self, state:SnapshotState) -> bool {
        let changed_state = match state {
            Clean | CleanPersisted => !matches!(self.state, Clean | CleanPersisted),
            Dirty | DirtyPersisted => !matches!(self.state, Dirty | DirtyPersisted),
            Comparing | ComparingPersisted => !matches!(self.state, Comparing | ComparingPersisted),
        };
        self.state = state;
        changed_state
    }

    pub fn set_to_slot(&mut self, slot: u8) {
        self.preset.index = slot;
        self.stash = None;
    }


    // Toggle the compare-state. If no action, return None, else return the new compare-state
    pub fn toggle_compare(&mut self, presets: &PresetDefs) -> Option<bool> {
        match self.state {
            Dirty | DirtyPersisted => {
                if let Some(p) = presets.active_entry() {
                    let dirty = std::mem::replace(&mut self.preset, p.clone());
                    self.stash = Some(dirty);
                    self.state = Comparing;
                    Some(true)
                } else {
                    None
                }
            },
            Comparing | ComparingPersisted => {
                if let Some(stashed) = self.stash.take() {
                    self.preset = stashed;
                }
                self.state = Dirty;
                Some(false)
            },
            _ => { None }
        }
    }


    /// Persist this state snapshot
    /// If Dirty or Comparing: persist to file. Change to DirtyPersisted or ComparingPersisted.
    /// If Clean: delete the state-file. Change to CleanPersisted.
    /// If action was taken, return Ok(true), else Ok(false). 
    pub fn persist_state(&mut self, path: &Path) -> Result<bool> {
        match self.state {
            Dirty => {
                self.state = DirtyPersisted;
                persist(&serde_json::to_value(&self)?, path)?;
                Ok(true)
            },
            Comparing => {
                self.state = ComparingPersisted;
                persist(&serde_json::to_value(&self)?, path)?;
                Ok(true)
            },
            Clean => {
                Self::remove_state_file(path)?;
                self.state = CleanPersisted;
                Ok(true)
            },
            _ => Ok(false)
        }   
    }

    pub fn remove_state_file(path: &Path) -> Result<()> {
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }

    /// Restore state if exists.
    /// If no file found, return Ok(None), if restore took place, Ok(Some(snapshot))
    pub fn restore_state(path: &Path) -> Result<Option<Self>> {
        if path.exists() {
            let snapshot: ConfigSnapshot = serde_json::from_value(load(&path)?)?;
            Ok(Some(snapshot))
        } else {
            Ok(None)
        }
    }
}