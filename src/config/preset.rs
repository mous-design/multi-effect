use serde::{Deserialize, Serialize};
use crate::{control::mapping::{ControllerDef}, engine::patch::ChainDef};

// ---------------------------------------------------------------------------
// PresetDef / PresetDefs
// ---------------------------------------------------------------------------
/// No active preset selected.
pub const PRESET_NONE: u8 = 255;

/// One numbered preset slot: a signal chain definition + controller mappings.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PresetDef {
    /// Preset number (0–127, matching MIDI program-change range, PRESET_NONE if unknown).
    pub index: u8,

    /// Chain defenitions for this preset.
    #[serde(default)]
    pub chains: Vec<ChainDef>,

    /// Controller bindings for this preset.
    /// Each entry references a device alias from `Config.control_devices` and supplies
    /// the key → target mappings for that device.
    #[serde(default)]
    pub controllers: Vec<ControllerDef>,
}

/// Collection of presets with an active-preset pointer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresetDefs {
    /// Current preset, if any. If none, values is PRESET_NONE
    #[serde(default = "PresetDefs::default_active")]
    pub active: u8,

    /// Preset definitions, sorted by index.
    #[serde(default)]
    pub items: Vec<PresetDef>,
}

impl Default for PresetDefs {
    fn default() -> Self {
        Self { items: Vec::new(), active: PresetDefs::default_active() }
    }
}

impl PresetDefs {
    fn default_active() -> u8 { PRESET_NONE }

    pub fn get(&self, index: u8) -> Option<&PresetDef> {
        self.items.iter().find(|p| p.index == index)
    }

    pub fn get_mut(&mut self, index: u8) -> Option<&mut PresetDef> {
        self.items.iter_mut().find(|p| p.index == index)
    }

    /// Active preset, or first preset as fallback.
    pub fn active_entry(&self) -> Option<&PresetDef> {
        self.get(self.active)
    }

    /// All preset indices (for listing).
    pub fn indices(&self) -> Vec<u8> {
        self.items.iter().map(|p| p.index).collect()
    }

    /// Update or replace a preset slot with the given chains and controllers.
    pub fn save_to_slot(&mut self, preset: PresetDef) {
        if let Some(current) = self.get_mut(preset.index) {
            *current = preset;
        } else {
            self.items.push(preset);
        }
    }

    /// Remove a preset by index. Returns true if it existed.
    pub fn remove_slot(&mut self, index: u8) -> bool {
        let len = self.items.len();
        self.items.retain(|p| p.index != index);
        self.items.len() < len
    }

    /// Remove controller bindings referencing `alias` from all presets.
    pub fn remove_device(&mut self, alias: &str) {
        for preset in &mut self.items {
            preset.controllers.retain(|c| c.device != alias);
        }
    }

    /// Rename a device alias in all preset controller bindings.
    pub fn rename_device(&mut self, old: &str, new: &str) {
        for preset in &mut self.items {
            for ctrl in &mut preset.controllers {
                if ctrl.device == old {
                    ctrl.device = new.to_string();
                }
            }
        }
    }
}
