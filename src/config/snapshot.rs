use std::path::Path;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use super::preset::PresetDefs;
use super::persist_fs::{persist, load, strip_derived};
use super::preset::PresetDef;
use super::{Config, ToPersistable, ToWire};
use crate::engine::device::ParamValue;

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

    /// Update a single node parameter. `param_path` = "node-key.param".
    /// Returns whether the value changed.
    ///
    /// **Strict typing**: rejects `Bool` for numeric params and numbers for
    /// `Bool` params with a warning. Numeric↔numeric (`Int`↔`Float`) is
    /// accepted to absorb untagged-serde round-trip non-determinism. CTRL
    /// paths that target a `Bool` param will arrive here as `Float` and be
    /// dropped — strict-policy boundary, hardware mappings to bools are not
    /// yet supported.
    pub fn apply_set(&mut self, param_path: &str, value: ParamValue) -> Result<bool> {
        use crate::engine::device::ParamType;
        let Some((node_key, param)) = param_path.split_once('.') else {
            bail!("invalid param path '{param_path}': missing '.'");
        };
        for chain in self.preset.chains.iter_mut() {
            for node in chain.nodes.iter_mut() {
                if node.key == node_key {
                    if let Some(info) = node.params_info.iter().find(|i| i.name == param) {
                        let compatible = matches!(
                            (&info.data_kind, value),
                            (ParamType::DiscreteBool { .. }, ParamValue::Bool(_))
                            | (ParamType::ContinuousFloat { .. }
                              | ParamType::ContinuousInt   { .. }
                              | ParamType::DiscreteFloat   { .. },
                               ParamValue::Float(_) | ParamValue::Int(_))
                        );
                        if !compatible {
                            tracing::warn!(
                                "SET {param_path}: type mismatch (declared {:?}, got {value:?}) — ignored",
                                info.data_kind
                            );
                            return Ok(false);
                        }
                    }
                    if node.params.get(param) != Some(&value) {
                        node.params.insert(param.to_string(), value);
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
                persist(&self.to_persistable()?, path)?;
                Ok(true)
            },
            Comparing => {
                self.state = ComparingPersisted;
                persist(&self.to_persistable()?, path)?;
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

impl ToPersistable for ConfigSnapshot {
    /// Disk shape: full state (including `stash`, needed to resume compare
    /// mode after restart) minus derived fields (`params_info`, recomputable
    /// from canonical + overrides).
    fn to_persistable(&self) -> Result<serde_json::Value> {
        let mut v = serde_json::to_value(self)?;
        strip_derived(&mut v);
        Ok(v)
    }
}

impl ToWire for ConfigSnapshot {
    /// Wire shape: everything a client renders against. Strips `stash` —
    /// it's master-internal scratchpad for compare mode; clients learn
    /// "comparing now" from `state` alone. Re-add by removing one line if a
    /// client ever needs the stashed preset.
    fn to_wire(&self) -> Result<serde_json::Value> {
        let mut v = serde_json::to_value(self)?;
        if let Some(obj) = v.as_object_mut() { obj.remove("stash"); }
        Ok(v)
    }
}