//! Core types for keyboard shortcuts

mod hotkey;
mod key;
mod modifiers;
mod tap_pattern;

pub use hotkey::{Hotkey, HotkeyEvent, HotkeyId, HotkeyState, KeyEvent};
pub use key::Key;
pub use modifiers::Modifiers;
pub use tap_pattern::{
    HandyKeysEvent, TapPattern, TapPatternEvent, TapPatternId, TapPatternMode, TriggerKey,
};
