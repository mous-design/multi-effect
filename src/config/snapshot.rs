use std::path::Path;
use anyhow::{Result, bail};
use serde_json::Value;
use serde::{Deserialize, Serialize};
use super::preset::PresetDefs;
use super::persist_fs::{persist, load};
use super::preset::PresetDef;
use super::Config;

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

/// Wire-format projection of [`ConfigSnapshot`]: everything a client needs to render,
/// without the internal `stash` field (which is only meaningful for persistence).
///
/// Borrows from the underlying snapshot — zero allocation. Produced by
/// [`ConfigSnapshot::to_view`].
#[derive(Serialize)]
pub struct SnapshotView<'a> {
    pub state:          &'a SnapshotState,
    pub preset:         &'a PresetDef,
    pub preset_indices: &'a [u8],
}

impl ConfigSnapshot {
    /// Borrowed projection for wire output — excludes the internal `stash` field.
    pub fn to_view(&self) -> SnapshotView<'_> {
        SnapshotView {
            state:          &self.state,
            preset:         &self.preset,
            preset_indices: &self.preset_indices,
        }
    }

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

    /// Update a single node parameter.  `param_path` = "node-key.param".
    pub fn apply_set(&mut self, param_path: &str, value: f32) -> Result<bool> {
        let Some((node_key, param)) = param_path.split_once('.') else {
            bail!("invalid param path '{param_path}': missing '.'");
        };
        for chain in self.preset.chains.iter_mut() {
            for node in chain.nodes.iter_mut() {
                if node.key == node_key {
                    node.params[param] = Value::from(value);
                    // If comparing, we can delete the stash
                    if matches!(self.state, Comparing | ComparingPersisted) {
                        self.stash = None;
                    }
                    return Ok(self.set_state(Dirty));

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