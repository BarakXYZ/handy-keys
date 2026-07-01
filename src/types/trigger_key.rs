//! Shared single-trigger definitions for tap-style recognizers.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

use super::hotkey::Hotkey;
use super::key::Key;
use super::modifiers::Modifiers;

/// One key-like input that can be tapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TriggerKey {
    Key(Key),
    Modifier(Modifiers),
}

impl TriggerKey {
    pub fn from_hotkey(hotkey: Hotkey) -> Result<Self> {
        match (hotkey.modifiers, hotkey.key) {
            (modifiers, Some(key)) if modifiers.is_empty() => Ok(Self::Key(key)),
            (modifiers, None) if is_single_concrete_modifier(modifiers) => {
                Ok(Self::Modifier(modifiers))
            }
            (modifiers, None) if !modifiers.is_empty() => Err(Error::InvalidTriggerKey(
                "tap trigger modifiers must be side-specific, e.g. shift_left".to_string(),
            )),
            _ => Err(Error::InvalidTriggerKey(
                "tap triggers must be exactly one key or one side-specific modifier".to_string(),
            )),
        }
    }
}

fn is_single_concrete_modifier(modifiers: Modifiers) -> bool {
    !modifiers.is_empty() && modifiers.bits().count_ones() == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_accepts_key_only_hotkey() {
        let hotkey: Hotkey = "d".parse().unwrap();
        assert_eq!(
            TriggerKey::from_hotkey(hotkey).unwrap(),
            TriggerKey::Key(Key::D)
        );
    }

    #[test]
    fn trigger_accepts_side_specific_modifier_only_hotkey() {
        let hotkey: Hotkey = "shift_left".parse().unwrap();
        assert_eq!(
            TriggerKey::from_hotkey(hotkey).unwrap(),
            TriggerKey::Modifier(Modifiers::SHIFT_LEFT)
        );
    }

    #[test]
    fn trigger_rejects_compound_modifier_only_hotkey() {
        let hotkey: Hotkey = "shift".parse().unwrap();
        assert!(matches!(
            TriggerKey::from_hotkey(hotkey),
            Err(Error::InvalidTriggerKey(_))
        ));
    }

    #[test]
    fn trigger_rejects_modified_key_hotkey() {
        let hotkey: Hotkey = "shift_left+d".parse().unwrap();
        assert!(matches!(
            TriggerKey::from_hotkey(hotkey),
            Err(Error::InvalidTriggerKey(_))
        ));
    }
}
