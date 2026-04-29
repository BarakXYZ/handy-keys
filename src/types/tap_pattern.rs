//! Tap-pattern definitions and validation.

use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::error::{Error, Result};

use super::hotkey::{Hotkey, HotkeyEvent};
use super::key::Key;
use super::modifiers::Modifiers;

/// A unique identifier for a registered tap pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TapPatternId(pub(crate) u32);

impl TapPatternId {
    pub fn as_u32(&self) -> u32 {
        self.0
    }

    pub fn from_u32(id: u32) -> Self {
        Self(id)
    }
}

/// One key-like input that can be tapped repeatedly.
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
            (modifiers, None) if !modifiers.is_empty() => Err(Error::InvalidTapPattern(
                "tap-pattern modifier triggers must be side-specific, e.g. shift_left".to_string(),
            )),
            _ => Err(Error::InvalidTapPattern(
                "tap-pattern triggers must be exactly one key or one side-specific modifier"
                    .to_string(),
            )),
        }
    }
}

/// Tap-pattern evaluation mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TapPatternMode {
    Lazy,
    TapThenHold,
}

/// A repeated-tap trigger. MVP validation accepts double-tap only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TapPattern {
    pub trigger: TriggerKey,
    pub tap_count: u8,
    pub timeout: Duration,
    pub mode: TapPatternMode,
}

impl TapPattern {
    pub fn double_tap(trigger: TriggerKey, timeout: Duration) -> Result<Self> {
        Self::new(trigger, 2, timeout)
    }

    pub fn double_tap_hold(trigger: TriggerKey, timeout: Duration) -> Result<Self> {
        Self::new_with_mode(trigger, 2, timeout, TapPatternMode::TapThenHold)
    }

    pub fn new(trigger: TriggerKey, tap_count: u8, timeout: Duration) -> Result<Self> {
        Self::new_with_mode(trigger, tap_count, timeout, TapPatternMode::Lazy)
    }

    pub fn new_with_mode(
        trigger: TriggerKey,
        tap_count: u8,
        timeout: Duration,
        mode: TapPatternMode,
    ) -> Result<Self> {
        if tap_count != 2 {
            return Err(Error::InvalidTapPattern(
                "only double-tap patterns are supported in this release".to_string(),
            ));
        }
        if timeout.is_zero() {
            return Err(Error::InvalidTapPattern(
                "tap-pattern timeout must be greater than zero".to_string(),
            ));
        }

        Ok(Self {
            trigger,
            tap_count,
            timeout,
            mode,
        })
    }
}

/// A tap-pattern trigger event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TapPatternEvent {
    pub id: TapPatternId,
    pub tap_count: u8,
    pub is_key_down: bool,
}

/// Unified event type for callers that want both hotkey and tap-pattern events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HandyKeysEvent {
    Hotkey(HotkeyEvent),
    TapPattern(TapPatternEvent),
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
    fn trigger_rejects_compound_modifier() {
        let hotkey: Hotkey = "shift".parse().unwrap();
        assert!(matches!(
            TriggerKey::from_hotkey(hotkey),
            Err(Error::InvalidTapPattern(_))
        ));
    }

    #[test]
    fn trigger_rejects_chord() {
        let hotkey: Hotkey = "ctrl+d".parse().unwrap();
        assert!(matches!(
            TriggerKey::from_hotkey(hotkey),
            Err(Error::InvalidTapPattern(_))
        ));
    }

    #[test]
    fn tap_pattern_requires_double_tap_for_mvp() {
        let trigger = TriggerKey::Key(Key::D);
        assert!(TapPattern::new(trigger, 1, Duration::from_millis(250)).is_err());
        assert!(TapPattern::new(trigger, 3, Duration::from_millis(250)).is_err());
        assert!(TapPattern::new(trigger, 2, Duration::ZERO).is_err());
        assert!(TapPattern::double_tap(trigger, Duration::from_millis(250)).is_ok());
        assert!(TapPattern::double_tap_hold(trigger, Duration::from_millis(250)).is_ok());
    }
}
