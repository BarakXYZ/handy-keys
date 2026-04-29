//! Pure tap-pattern recognizer.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use crate::error::{Error, Result};
use crate::types::{KeyEvent, TapPattern, TapPatternEvent, TapPatternId, TriggerKey};

#[derive(Debug, Clone, Copy)]
struct ActiveTapSequence {
    trigger: TriggerKey,
    count: u8,
    last_tap_at: Instant,
}

/// Recognizes repeated taps from raw HandyKeys key events.
#[derive(Debug)]
pub struct TapPatternRecognizer {
    patterns: HashMap<TapPatternId, TapPattern>,
    active: Option<ActiveTapSequence>,
    held_triggers: HashSet<TriggerKey>,
    next_id: u32,
}

impl Default for TapPatternRecognizer {
    fn default() -> Self {
        Self {
            patterns: HashMap::new(),
            active: None,
            held_triggers: HashSet::new(),
            next_id: 1,
        }
    }
}

impl TapPatternRecognizer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, pattern: TapPattern) -> Result<TapPatternId> {
        let id = TapPatternId::from_u32(self.next_id);
        self.next_id = self.next_id.saturating_add(1).max(1);
        self.register_with_id(id, pattern)?;
        Ok(id)
    }

    pub fn register_with_id(&mut self, id: TapPatternId, pattern: TapPattern) -> Result<()> {
        if self.patterns.contains_key(&id) {
            return Err(Error::TapPatternAlreadyRegistered(format!("id {:?}", id)));
        }

        if self
            .patterns
            .values()
            .any(|existing| existing.trigger == pattern.trigger)
        {
            return Err(Error::TapPatternAlreadyRegistered(format!(
                "trigger {:?}",
                pattern.trigger
            )));
        }

        self.next_id = self.next_id.max(id.as_u32().saturating_add(1));
        self.patterns.insert(id, pattern);
        Ok(())
    }

    pub fn unregister(&mut self, id: TapPatternId) -> Result<()> {
        if self.patterns.remove(&id).is_none() {
            return Err(Error::TapPatternNotFound(id));
        }
        Ok(())
    }

    pub fn get(&self, id: TapPatternId) -> Option<TapPattern> {
        self.patterns.get(&id).copied()
    }

    pub fn count(&self) -> usize {
        self.patterns.len()
    }

    pub fn process_event_at(&mut self, event: &KeyEvent, now: Instant) -> Vec<TapPatternEvent> {
        let Some(trigger) = trigger_from_event(event) else {
            return Vec::new();
        };

        if !event.is_key_down {
            self.held_triggers.remove(&trigger);
            return Vec::new();
        }

        if !self.held_triggers.insert(trigger) {
            return Vec::new();
        }

        let Some((id, pattern)) = self
            .patterns
            .iter()
            .find(|(_, pattern)| pattern.trigger == trigger)
            .map(|(id, pattern)| (*id, *pattern))
        else {
            self.interrupt_if_different(trigger);
            return Vec::new();
        };

        let count = match self.active {
            Some(active) if active.trigger == trigger => {
                let within_timeout = now
                    .checked_duration_since(active.last_tap_at)
                    .map(|elapsed| elapsed <= pattern.timeout)
                    .unwrap_or(false);
                if within_timeout {
                    active.count.saturating_add(1)
                } else {
                    1
                }
            }
            Some(_) | None => 1,
        };

        if count >= pattern.tap_count {
            self.active = None;
            return vec![TapPatternEvent {
                id,
                tap_count: count,
            }];
        }

        self.active = Some(ActiveTapSequence {
            trigger,
            count,
            last_tap_at: now,
        });
        Vec::new()
    }

    fn interrupt_if_different(&mut self, trigger: TriggerKey) {
        if self
            .active
            .map(|active| active.trigger != trigger)
            .unwrap_or(false)
        {
            self.active = None;
        }
    }
}

fn trigger_from_event(event: &KeyEvent) -> Option<TriggerKey> {
    event
        .key
        .map(TriggerKey::Key)
        .or_else(|| event.changed_modifier.map(TriggerKey::Modifier))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Key, Modifiers};
    use std::time::{Duration, Instant};

    fn key_event(key: Key, is_key_down: bool) -> KeyEvent {
        KeyEvent {
            modifiers: Modifiers::empty(),
            key: Some(key),
            is_key_down,
            changed_modifier: None,
        }
    }

    fn modifier_event(modifier: Modifiers, is_key_down: bool) -> KeyEvent {
        KeyEvent {
            modifiers: if is_key_down {
                modifier
            } else {
                Modifiers::empty()
            },
            key: None,
            is_key_down,
            changed_modifier: Some(modifier),
        }
    }

    fn double_tap_pattern(trigger: TriggerKey) -> TapPattern {
        TapPattern::double_tap(trigger, Duration::from_millis(250)).unwrap()
    }

    #[test]
    fn double_tap_within_timeout_emits_once() {
        let start = Instant::now();
        let mut recognizer = TapPatternRecognizer::new();
        let id = recognizer
            .register(double_tap_pattern(TriggerKey::Key(Key::D)))
            .unwrap();

        assert!(recognizer
            .process_event_at(&key_event(Key::D, true), start)
            .is_empty());
        recognizer.process_event_at(&key_event(Key::D, false), start);

        let events = recognizer
            .process_event_at(&key_event(Key::D, true), start + Duration::from_millis(120));
        assert_eq!(events, vec![TapPatternEvent { id, tap_count: 2 }]);

        recognizer.process_event_at(&key_event(Key::D, false), start);
        assert!(recognizer
            .process_event_at(&key_event(Key::D, true), start + Duration::from_millis(140))
            .is_empty());
    }

    #[test]
    fn slow_second_tap_does_not_emit() {
        let start = Instant::now();
        let mut recognizer = TapPatternRecognizer::new();
        recognizer
            .register(double_tap_pattern(TriggerKey::Key(Key::D)))
            .unwrap();

        recognizer.process_event_at(&key_event(Key::D, true), start);
        recognizer.process_event_at(&key_event(Key::D, false), start);
        assert!(recognizer
            .process_event_at(&key_event(Key::D, true), start + Duration::from_millis(300))
            .is_empty());
    }

    #[test]
    fn different_trigger_interrupts_sequence() {
        let start = Instant::now();
        let mut recognizer = TapPatternRecognizer::new();
        recognizer
            .register(double_tap_pattern(TriggerKey::Key(Key::D)))
            .unwrap();

        recognizer.process_event_at(&key_event(Key::D, true), start);
        recognizer.process_event_at(&key_event(Key::D, false), start);
        recognizer.process_event_at(&key_event(Key::A, true), start + Duration::from_millis(20));
        recognizer.process_event_at(&key_event(Key::A, false), start + Duration::from_millis(30));

        assert!(recognizer
            .process_event_at(&key_event(Key::D, true), start + Duration::from_millis(40))
            .is_empty());
    }

    #[test]
    fn key_repeat_while_held_does_not_count_as_tap() {
        let start = Instant::now();
        let mut recognizer = TapPatternRecognizer::new();
        recognizer
            .register(double_tap_pattern(TriggerKey::Key(Key::D)))
            .unwrap();

        recognizer.process_event_at(&key_event(Key::D, true), start);
        assert!(recognizer
            .process_event_at(&key_event(Key::D, true), start + Duration::from_millis(10))
            .is_empty());
    }

    #[test]
    fn modifier_release_does_not_count_as_tap() {
        let start = Instant::now();
        let mut recognizer = TapPatternRecognizer::new();
        recognizer
            .register(double_tap_pattern(TriggerKey::Modifier(
                Modifiers::SHIFT_LEFT,
            )))
            .unwrap();

        recognizer.process_event_at(&modifier_event(Modifiers::SHIFT_LEFT, true), start);
        assert!(recognizer
            .process_event_at(
                &modifier_event(Modifiers::SHIFT_LEFT, false),
                start + Duration::from_millis(40)
            )
            .is_empty());
    }

    #[test]
    fn different_registered_triggers_do_not_interfere() {
        let start = Instant::now();
        let mut recognizer = TapPatternRecognizer::new();
        let d_id = recognizer
            .register(double_tap_pattern(TriggerKey::Key(Key::D)))
            .unwrap();
        let f_id = recognizer
            .register(double_tap_pattern(TriggerKey::Key(Key::F)))
            .unwrap();

        recognizer.process_event_at(&key_event(Key::D, true), start);
        recognizer.process_event_at(&key_event(Key::D, false), start);
        recognizer.process_event_at(&key_event(Key::F, true), start + Duration::from_millis(20));
        recognizer.process_event_at(&key_event(Key::F, false), start + Duration::from_millis(30));
        let f_events = recognizer
            .process_event_at(&key_event(Key::F, true), start + Duration::from_millis(40));
        assert_eq!(
            f_events,
            vec![TapPatternEvent {
                id: f_id,
                tap_count: 2
            }]
        );

        recognizer.process_event_at(&key_event(Key::F, false), start + Duration::from_millis(50));
        recognizer.process_event_at(&key_event(Key::D, true), start + Duration::from_millis(60));
        recognizer.process_event_at(&key_event(Key::D, false), start + Duration::from_millis(70));
        let d_events = recognizer
            .process_event_at(&key_event(Key::D, true), start + Duration::from_millis(80));
        assert_eq!(
            d_events,
            vec![TapPatternEvent {
                id: d_id,
                tap_count: 2
            }]
        );
    }

    #[test]
    fn duplicate_trigger_registration_is_rejected() {
        let mut recognizer = TapPatternRecognizer::new();
        let pattern = double_tap_pattern(TriggerKey::Key(Key::D));
        recognizer.register(pattern).unwrap();
        assert!(matches!(
            recognizer.register(pattern),
            Err(Error::TapPatternAlreadyRegistered(_))
        ));
    }
}
