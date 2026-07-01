//! Tap-pattern definitions and validation.

use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::error::{Error, Result};

use super::trigger_key::TriggerKey;

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

#[cfg(test)]
mod tests {
    use super::super::Key;
    use super::*;

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
