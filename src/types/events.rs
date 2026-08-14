//! Unified HandyKeys events.

use serde::{Deserialize, Serialize};

use super::hotkey::HotkeyEvent;
use super::input_activity::InputActivity;
use super::tap_alone::TapAloneEvent;
use super::tap_pattern::TapPatternEvent;

/// Unified event type for callers that want all HandyKeys events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HandyKeysEvent {
    Hotkey(HotkeyEvent),
    TapPattern(TapPatternEvent),
    TapAlone(TapAloneEvent),
    InputActivity(InputActivity),
}
