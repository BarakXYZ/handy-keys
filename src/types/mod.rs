//! Core types for keyboard shortcuts

mod events;
mod hotkey;
mod input_activity;
mod key;
mod modifiers;
mod tap_alone;
mod tap_pattern;
mod trigger_key;

pub use events::HandyKeysEvent;
pub use hotkey::{Hotkey, HotkeyEvent, HotkeyId, HotkeyState, KeyEvent};
pub use input_activity::{InputActivity, MouseButtonActivity};
pub use key::Key;
pub use modifiers::Modifiers;
pub use tap_alone::{TapAlone, TapAloneEvent, TapAloneId};
pub use tap_pattern::{TapPattern, TapPatternEvent, TapPatternId, TapPatternMode};
pub use trigger_key::TriggerKey;
