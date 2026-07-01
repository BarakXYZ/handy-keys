//! Tap-alone trigger definitions and validation.

use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::error::{Error, Result};

use super::{key::Key, trigger_key::TriggerKey};

/// A unique identifier for a registered tap-alone trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TapAloneId(pub(crate) u32);

impl TapAloneId {
    pub fn as_u32(&self) -> u32 {
        self.0
    }

    pub fn from_u32(id: u32) -> Self {
        Self(id)
    }
}

/// A trigger that fires after one short, uninterrupted press and release.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TapAlone {
    pub trigger: TriggerKey,
    pub max_press_duration: Duration,
    pub double_tap_suppression: Duration,
}

impl TapAlone {
    pub fn new(
        trigger: TriggerKey,
        max_press_duration: Duration,
        double_tap_suppression: Duration,
    ) -> Result<Self> {
        if let TriggerKey::Key(key) = trigger {
            if is_mouse_button(key) {
                return Err(Error::InvalidTapAlone(
                    "tap-alone mouse button triggers are not supported".to_string(),
                ));
            }
        }
        if max_press_duration.is_zero() {
            return Err(Error::InvalidTapAlone(
                "tap-alone max press duration must be greater than zero".to_string(),
            ));
        }
        if double_tap_suppression.is_zero() {
            return Err(Error::InvalidTapAlone(
                "tap-alone double-tap suppression must be greater than zero".to_string(),
            ));
        }
        Ok(Self {
            trigger,
            max_press_duration,
            double_tap_suppression,
        })
    }
}

fn is_mouse_button(key: Key) -> bool {
    matches!(
        key,
        Key::MouseLeft | Key::MouseRight | Key::MouseMiddle | Key::MouseX1 | Key::MouseX2
    )
}

/// Event emitted when a tap-alone trigger resolves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TapAloneEvent {
    pub id: TapAloneId,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Key;
    use std::time::Duration;

    #[test]
    fn tap_alone_requires_positive_timeouts() {
        let trigger = TriggerKey::Key(Key::D);
        assert!(TapAlone::new(
            trigger,
            Duration::from_millis(1000),
            Duration::from_millis(250),
        )
        .is_ok());
        assert!(TapAlone::new(trigger, Duration::ZERO, Duration::from_millis(250)).is_err());
        assert!(TapAlone::new(trigger, Duration::from_millis(1000), Duration::ZERO).is_err());
    }

    #[test]
    fn tap_alone_keeps_trigger_and_timeouts() {
        let trigger = TriggerKey::Key(Key::D);
        let tap_alone = TapAlone::new(
            trigger,
            Duration::from_millis(999),
            Duration::from_millis(251),
        )
        .unwrap();

        assert_eq!(tap_alone.trigger, trigger);
        assert_eq!(tap_alone.max_press_duration, Duration::from_millis(999));
        assert_eq!(tap_alone.double_tap_suppression, Duration::from_millis(251));
    }

    #[test]
    fn tap_alone_rejects_mouse_button_triggers() {
        for key in [
            Key::MouseLeft,
            Key::MouseRight,
            Key::MouseMiddle,
            Key::MouseX1,
            Key::MouseX2,
        ] {
            assert!(matches!(
                TapAlone::new(
                    TriggerKey::Key(key),
                    Duration::from_millis(1000),
                    Duration::from_millis(250),
                ),
                Err(Error::InvalidTapAlone(_))
            ));
        }
    }
}
