//! Pure tap-alone recognizer.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use crate::error::{Error, Result};
use crate::types::{
    InputActivity, KeyEvent, Modifiers, TapAlone, TapAloneEvent, TapAloneId, TriggerKey,
};

#[derive(Debug, Clone, Copy)]
struct ActivePress {
    id: TapAloneId,
    pattern: TapAlone,
    started_at: Instant,
    interrupted: bool,
}

#[derive(Debug, Clone, Copy)]
struct PendingRelease {
    id: TapAloneId,
    emit_at: Instant,
}

/// Recognizes short, uninterrupted single taps from raw HandyKeys key events.
#[derive(Debug)]
pub struct TapAloneRecognizer {
    patterns: HashMap<TapAloneId, TapAlone>,
    next_id: u32,
    held_triggers: HashSet<TriggerKey>,
    active_presses: HashMap<TriggerKey, ActivePress>,
    pending_releases: HashMap<TriggerKey, PendingRelease>,
    suppressed_until_release: HashSet<TriggerKey>,
}

impl Default for TapAloneRecognizer {
    fn default() -> Self {
        Self {
            patterns: HashMap::new(),
            next_id: 1,
            held_triggers: HashSet::new(),
            active_presses: HashMap::new(),
            pending_releases: HashMap::new(),
            suppressed_until_release: HashSet::new(),
        }
    }
}

impl TapAloneRecognizer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, pattern: TapAlone) -> Result<TapAloneId> {
        let id = TapAloneId::from_u32(self.next_id);
        self.next_id = self.next_id.saturating_add(1).max(1);
        self.register_with_id(id, pattern)?;
        Ok(id)
    }

    pub fn register_with_id(&mut self, id: TapAloneId, pattern: TapAlone) -> Result<()> {
        if self.patterns.contains_key(&id) {
            return Err(Error::TapAloneAlreadyRegistered(format!("id {:?}", id)));
        }

        if self
            .patterns
            .values()
            .any(|existing| existing.trigger == pattern.trigger)
        {
            return Err(Error::TapAloneAlreadyRegistered(format!(
                "trigger {:?}",
                pattern.trigger
            )));
        }

        self.next_id = self.next_id.max(id.as_u32().saturating_add(1)).max(1);
        self.patterns.insert(id, pattern);
        Ok(())
    }

    pub fn unregister(&mut self, id: TapAloneId) -> Result<()> {
        let Some(pattern) = self.patterns.remove(&id) else {
            return Err(Error::TapAloneNotFound(id));
        };

        self.active_presses.remove(&pattern.trigger);
        self.pending_releases.remove(&pattern.trigger);
        self.suppressed_until_release.remove(&pattern.trigger);
        Ok(())
    }

    pub fn get(&self, id: TapAloneId) -> Option<TapAlone> {
        self.patterns.get(&id).copied()
    }

    pub fn count(&self) -> usize {
        self.patterns.len()
    }

    pub fn next_deadline_at(&self) -> Option<Instant> {
        self.pending_releases
            .values()
            .map(|pending| pending.emit_at)
            .min()
    }

    pub fn process_event_at(&mut self, event: &KeyEvent, now: Instant) -> Vec<TapAloneEvent> {
        let results = self.drain_expired_at(now);

        if is_interruption_activity(event) {
            self.interrupt_all_active();
            return results;
        }

        let Some(trigger) = trigger_from_event(event) else {
            return results;
        };

        if event.is_key_down {
            self.interrupt_active_except(trigger);

            if self.held_triggers.contains(&trigger) {
                if modifier_snapshot_interrupts(event.modifiers, trigger) {
                    if let Some(active) = self.active_presses.get_mut(&trigger) {
                        active.interrupted = true;
                    }
                }
                return results;
            }

            if self.pending_releases.remove(&trigger).is_some() {
                self.suppressed_until_release.insert(trigger);
                self.held_triggers.insert(trigger);
                return results;
            }

            let interrupted = self.held_triggers.iter().any(|held| *held != trigger)
                || modifier_snapshot_interrupts(event.modifiers, trigger);
            self.held_triggers.insert(trigger);

            if let Some((id, pattern)) = self.pattern_for_trigger(trigger) {
                self.active_presses.insert(
                    trigger,
                    ActivePress {
                        id,
                        pattern,
                        started_at: now,
                        interrupted,
                    },
                );
            }
            return results;
        }

        self.interrupt_active_except(trigger);
        self.held_triggers.remove(&trigger);

        if self.suppressed_until_release.remove(&trigger) {
            return results;
        }

        if let Some(mut active) = self.active_presses.remove(&trigger) {
            active.interrupted |= modifier_snapshot_interrupts(event.modifiers, trigger);
            let press_duration = now.saturating_duration_since(active.started_at);
            if !active.interrupted && press_duration < active.pattern.max_press_duration {
                self.pending_releases.insert(
                    trigger,
                    PendingRelease {
                        id: active.id,
                        emit_at: now + active.pattern.double_tap_suppression,
                    },
                );
            }
        }

        results
    }

    pub fn drain_expired_at(&mut self, now: Instant) -> Vec<TapAloneEvent> {
        let mut expired: Vec<(TriggerKey, PendingRelease)> = self
            .pending_releases
            .iter()
            .filter_map(|(trigger, pending)| {
                (pending.emit_at <= now).then_some((*trigger, *pending))
            })
            .collect();

        expired.sort_by_key(|(_, pending)| (pending.emit_at, pending.id.as_u32()));

        expired
            .into_iter()
            .filter_map(|(trigger, _)| {
                self.pending_releases
                    .remove(&trigger)
                    .map(|pending| TapAloneEvent { id: pending.id })
            })
            .collect()
    }

    fn pattern_for_trigger(&self, trigger: TriggerKey) -> Option<(TapAloneId, TapAlone)> {
        self.patterns
            .iter()
            .find(|(_, pattern)| pattern.trigger == trigger)
            .map(|(id, pattern)| (*id, *pattern))
    }

    fn interrupt_active_except(&mut self, trigger: TriggerKey) {
        for (active_trigger, active) in self.active_presses.iter_mut() {
            if *active_trigger != trigger {
                active.interrupted = true;
            }
        }
    }

    fn interrupt_all_active(&mut self) {
        for active in self.active_presses.values_mut() {
            active.interrupted = true;
        }
    }
}

fn trigger_from_event(event: &KeyEvent) -> Option<TriggerKey> {
    event
        .key
        .map(TriggerKey::Key)
        .or_else(|| event.changed_modifier.map(TriggerKey::Modifier))
}

fn is_interruption_activity(event: &KeyEvent) -> bool {
    matches!(
        event.activity,
        Some(InputActivity::MouseButtonDown(_)) | Some(InputActivity::ScrollWheel)
    )
}

fn modifier_snapshot_interrupts(modifiers: Modifiers, trigger: TriggerKey) -> bool {
    match trigger {
        TriggerKey::Key(_) => !modifiers.is_empty(),
        TriggerKey::Modifier(trigger_modifier) => !(modifiers & !trigger_modifier).is_empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{InputActivity, Key, Modifiers, MouseButtonActivity};
    use std::time::Duration;

    fn key_event(key: Key, is_key_down: bool) -> KeyEvent {
        KeyEvent {
            modifiers: Modifiers::empty(),
            key: Some(key),
            is_key_down,
            changed_modifier: None,
            activity: None,
        }
    }

    fn key_event_with_modifiers(key: Key, is_key_down: bool, modifiers: Modifiers) -> KeyEvent {
        KeyEvent {
            modifiers,
            key: Some(key),
            is_key_down,
            changed_modifier: None,
            activity: None,
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
            activity: None,
        }
    }

    fn modifier_event_with_modifiers(
        modifier: Modifiers,
        is_key_down: bool,
        modifiers: Modifiers,
    ) -> KeyEvent {
        KeyEvent {
            modifiers,
            key: None,
            is_key_down,
            changed_modifier: Some(modifier),
            activity: None,
        }
    }

    fn activity_event(activity: InputActivity) -> KeyEvent {
        KeyEvent {
            modifiers: Modifiers::empty(),
            key: None,
            is_key_down: true,
            changed_modifier: None,
            activity: Some(activity),
        }
    }

    fn tap_alone(trigger: TriggerKey) -> TapAlone {
        TapAlone::new(
            trigger,
            Duration::from_millis(1000),
            Duration::from_millis(250),
        )
        .unwrap()
    }

    #[test]
    fn tap_alone_emits_after_release_and_suppression_deadline() {
        let start = Instant::now();
        let mut recognizer = TapAloneRecognizer::new();
        let id = recognizer
            .register(tap_alone(TriggerKey::Key(Key::D)))
            .unwrap();

        assert!(recognizer
            .process_event_at(&key_event(Key::D, true), start)
            .is_empty());
        assert!(recognizer
            .process_event_at(&key_event(Key::D, false), start + Duration::from_millis(40))
            .is_empty());
        assert!(recognizer
            .drain_expired_at(start + Duration::from_millis(289))
            .is_empty());
        assert_eq!(
            recognizer.drain_expired_at(start + Duration::from_millis(290)),
            vec![TapAloneEvent { id }]
        );
    }

    #[test]
    fn tap_alone_cancels_when_other_key_is_pressed_while_held() {
        let start = Instant::now();
        let mut recognizer = TapAloneRecognizer::new();
        recognizer
            .register(tap_alone(TriggerKey::Key(Key::D)))
            .unwrap();

        recognizer.process_event_at(&key_event(Key::D, true), start);
        recognizer.process_event_at(&key_event(Key::F, true), start + Duration::from_millis(10));
        recognizer.process_event_at(&key_event(Key::D, false), start + Duration::from_millis(20));

        assert!(recognizer
            .drain_expired_at(start + Duration::from_millis(300))
            .is_empty());
    }

    #[test]
    fn tap_alone_cancels_when_other_modifier_changes_while_held() {
        let start = Instant::now();
        let mut recognizer = TapAloneRecognizer::new();
        recognizer
            .register(tap_alone(TriggerKey::Modifier(Modifiers::CTRL_LEFT)))
            .unwrap();

        recognizer.process_event_at(&modifier_event(Modifiers::CTRL_LEFT, true), start);
        recognizer.process_event_at(
            &modifier_event(Modifiers::SHIFT_RIGHT, true),
            start + Duration::from_millis(10),
        );
        recognizer.process_event_at(
            &modifier_event(Modifiers::CTRL_LEFT, false),
            start + Duration::from_millis(20),
        );

        assert!(recognizer
            .drain_expired_at(start + Duration::from_millis(300))
            .is_empty());
    }

    #[test]
    fn tap_alone_cancels_when_key_event_modifier_snapshot_is_chorded() {
        let start = Instant::now();
        let mut recognizer = TapAloneRecognizer::new();
        recognizer
            .register(tap_alone(TriggerKey::Key(Key::D)))
            .unwrap();

        recognizer.process_event_at(
            &key_event_with_modifiers(Key::D, true, Modifiers::CTRL_LEFT),
            start,
        );
        recognizer.process_event_at(
            &key_event_with_modifiers(Key::D, false, Modifiers::CTRL_LEFT),
            start + Duration::from_millis(20),
        );

        assert!(recognizer
            .drain_expired_at(start + Duration::from_millis(300))
            .is_empty());
    }

    #[test]
    fn tap_alone_cancels_when_modifier_event_snapshot_has_another_modifier() {
        let start = Instant::now();
        let mut recognizer = TapAloneRecognizer::new();
        recognizer
            .register(tap_alone(TriggerKey::Modifier(Modifiers::CTRL_LEFT)))
            .unwrap();

        recognizer.process_event_at(
            &modifier_event_with_modifiers(
                Modifiers::CTRL_LEFT,
                true,
                Modifiers::CTRL_LEFT | Modifiers::SHIFT_RIGHT,
            ),
            start,
        );
        recognizer.process_event_at(
            &modifier_event_with_modifiers(Modifiers::CTRL_LEFT, false, Modifiers::SHIFT_RIGHT),
            start + Duration::from_millis(20),
        );

        assert!(recognizer
            .drain_expired_at(start + Duration::from_millis(300))
            .is_empty());
    }

    #[test]
    fn tap_alone_cancels_when_key_release_modifier_snapshot_is_chorded() {
        let start = Instant::now();
        let mut recognizer = TapAloneRecognizer::new();
        recognizer
            .register(tap_alone(TriggerKey::Key(Key::D)))
            .unwrap();

        recognizer.process_event_at(&key_event(Key::D, true), start);
        recognizer.process_event_at(
            &key_event_with_modifiers(Key::D, false, Modifiers::CTRL_LEFT),
            start + Duration::from_millis(20),
        );

        assert!(recognizer
            .drain_expired_at(start + Duration::from_millis(300))
            .is_empty());
    }

    #[test]
    fn tap_alone_cancels_when_mouse_button_or_scroll_happens_while_held() {
        let start = Instant::now();
        for activity in [
            InputActivity::MouseButtonDown(MouseButtonActivity::Left),
            InputActivity::ScrollWheel,
        ] {
            let mut recognizer = TapAloneRecognizer::new();
            recognizer
                .register(tap_alone(TriggerKey::Key(Key::D)))
                .unwrap();
            recognizer.process_event_at(&key_event(Key::D, true), start);
            recognizer
                .process_event_at(&activity_event(activity), start + Duration::from_millis(10));
            recognizer
                .process_event_at(&key_event(Key::D, false), start + Duration::from_millis(20));
            assert!(recognizer
                .drain_expired_at(start + Duration::from_millis(300))
                .is_empty());
        }
    }

    #[test]
    fn tap_alone_uses_strict_max_press_boundary_like_karabiner() {
        let start = Instant::now();
        let trigger = TriggerKey::Key(Key::D);

        let mut emits = TapAloneRecognizer::new();
        let id = emits.register(tap_alone(trigger)).unwrap();
        emits.process_event_at(&key_event(Key::D, true), start);
        emits.process_event_at(
            &key_event(Key::D, false),
            start + Duration::from_millis(999),
        );
        assert_eq!(
            emits.drain_expired_at(start + Duration::from_millis(1249)),
            vec![TapAloneEvent { id }]
        );

        for release_at in [1000, 1001] {
            let mut recognizer = TapAloneRecognizer::new();
            recognizer.register(tap_alone(trigger)).unwrap();
            recognizer.process_event_at(&key_event(Key::D, true), start);
            recognizer.process_event_at(
                &key_event(Key::D, false),
                start + Duration::from_millis(release_at),
            );
            assert!(recognizer
                .drain_expired_at(start + Duration::from_millis(1300))
                .is_empty());
        }
    }

    #[test]
    fn tap_alone_ignores_key_repeat_while_held() {
        let start = Instant::now();
        let mut recognizer = TapAloneRecognizer::new();
        let id = recognizer
            .register(tap_alone(TriggerKey::Key(Key::D)))
            .unwrap();

        recognizer.process_event_at(&key_event(Key::D, true), start);
        recognizer.process_event_at(&key_event(Key::D, true), start + Duration::from_millis(10));
        recognizer.process_event_at(&key_event(Key::D, false), start + Duration::from_millis(20));

        assert_eq!(
            recognizer.drain_expired_at(start + Duration::from_millis(270)),
            vec![TapAloneEvent { id }]
        );
    }

    #[test]
    fn tap_alone_key_repeat_with_modifier_snapshot_interrupts_active_press() {
        let start = Instant::now();
        let mut recognizer = TapAloneRecognizer::new();
        recognizer
            .register(tap_alone(TriggerKey::Key(Key::D)))
            .unwrap();

        recognizer.process_event_at(&key_event(Key::D, true), start);
        recognizer.process_event_at(
            &key_event_with_modifiers(Key::D, true, Modifiers::CTRL_LEFT),
            start + Duration::from_millis(10),
        );
        recognizer.process_event_at(&key_event(Key::D, false), start + Duration::from_millis(20));

        assert!(recognizer
            .drain_expired_at(start + Duration::from_millis(300))
            .is_empty());
    }

    #[test]
    fn tap_alone_same_trigger_second_tap_suppresses_both_single_taps() {
        let start = Instant::now();
        let mut recognizer = TapAloneRecognizer::new();
        recognizer
            .register(tap_alone(TriggerKey::Key(Key::D)))
            .unwrap();

        recognizer.process_event_at(&key_event(Key::D, true), start);
        recognizer.process_event_at(&key_event(Key::D, false), start + Duration::from_millis(20));
        recognizer.process_event_at(&key_event(Key::D, true), start + Duration::from_millis(100));
        recognizer.process_event_at(
            &key_event(Key::D, false),
            start + Duration::from_millis(120),
        );

        assert!(recognizer
            .drain_expired_at(start + Duration::from_millis(400))
            .is_empty());
    }

    #[test]
    fn tap_alone_third_isolated_tap_after_suppressed_double_tap_emits() {
        let start = Instant::now();
        let mut recognizer = TapAloneRecognizer::new();
        let id = recognizer
            .register(tap_alone(TriggerKey::Key(Key::D)))
            .unwrap();

        recognizer.process_event_at(&key_event(Key::D, true), start);
        recognizer.process_event_at(&key_event(Key::D, false), start + Duration::from_millis(20));
        recognizer.process_event_at(&key_event(Key::D, true), start + Duration::from_millis(100));
        recognizer.process_event_at(
            &key_event(Key::D, false),
            start + Duration::from_millis(120),
        );
        recognizer.process_event_at(&key_event(Key::D, true), start + Duration::from_millis(500));
        recognizer.process_event_at(
            &key_event(Key::D, false),
            start + Duration::from_millis(520),
        );

        assert_eq!(
            recognizer.drain_expired_at(start + Duration::from_millis(770)),
            vec![TapAloneEvent { id }]
        );
    }

    #[test]
    fn tap_alone_other_key_after_release_does_not_cancel_pending_single() {
        let start = Instant::now();
        let mut recognizer = TapAloneRecognizer::new();
        let id = recognizer
            .register(tap_alone(TriggerKey::Key(Key::D)))
            .unwrap();

        recognizer.process_event_at(&key_event(Key::D, true), start);
        recognizer.process_event_at(&key_event(Key::D, false), start + Duration::from_millis(20));
        recognizer.process_event_at(&key_event(Key::F, true), start + Duration::from_millis(30));

        assert_eq!(
            recognizer.drain_expired_at(start + Duration::from_millis(270)),
            vec![TapAloneEvent { id }]
        );
    }

    #[test]
    fn tap_alone_expired_pending_emits_before_late_same_trigger_down() {
        let start = Instant::now();
        let mut recognizer = TapAloneRecognizer::new();
        let id = recognizer
            .register(tap_alone(TriggerKey::Key(Key::D)))
            .unwrap();

        recognizer.process_event_at(&key_event(Key::D, true), start);
        recognizer.process_event_at(&key_event(Key::D, false), start + Duration::from_millis(20));

        assert_eq!(
            recognizer
                .process_event_at(&key_event(Key::D, true), start + Duration::from_millis(300)),
            vec![TapAloneEvent { id }]
        );
    }

    #[test]
    fn tap_alone_multiple_pending_releases_emit_independently() {
        let start = Instant::now();
        let mut recognizer = TapAloneRecognizer::new();
        let d_id = recognizer
            .register(tap_alone(TriggerKey::Key(Key::D)))
            .unwrap();
        let f_id = recognizer
            .register(tap_alone(TriggerKey::Key(Key::F)))
            .unwrap();

        recognizer.process_event_at(&key_event(Key::D, true), start);
        recognizer.process_event_at(&key_event(Key::D, false), start + Duration::from_millis(20));
        recognizer.process_event_at(&key_event(Key::F, true), start + Duration::from_millis(40));
        recognizer.process_event_at(&key_event(Key::F, false), start + Duration::from_millis(60));

        assert_eq!(
            recognizer.drain_expired_at(start + Duration::from_millis(270)),
            vec![TapAloneEvent { id: d_id }]
        );
        assert_eq!(
            recognizer.drain_expired_at(start + Duration::from_millis(310)),
            vec![TapAloneEvent { id: f_id }]
        );
    }

    #[test]
    fn tap_alone_multiple_expired_releases_emit_by_deadline_order() {
        let start = Instant::now();
        let mut recognizer = TapAloneRecognizer::new();
        let d_id = recognizer
            .register(tap_alone(TriggerKey::Key(Key::D)))
            .unwrap();
        let f_id = recognizer
            .register(tap_alone(TriggerKey::Key(Key::F)))
            .unwrap();

        recognizer.process_event_at(&key_event(Key::F, true), start);
        recognizer.process_event_at(&key_event(Key::F, false), start + Duration::from_millis(20));
        recognizer.process_event_at(&key_event(Key::D, true), start + Duration::from_millis(40));
        recognizer.process_event_at(&key_event(Key::D, false), start + Duration::from_millis(60));

        assert_eq!(
            recognizer.drain_expired_at(start + Duration::from_millis(400)),
            vec![TapAloneEvent { id: f_id }, TapAloneEvent { id: d_id }]
        );
    }

    #[test]
    fn tap_alone_target_pressed_while_another_trigger_is_held_is_interrupted() {
        let start = Instant::now();
        let mut recognizer = TapAloneRecognizer::new();
        recognizer
            .register(tap_alone(TriggerKey::Key(Key::D)))
            .unwrap();

        recognizer.process_event_at(&key_event(Key::F, true), start);
        recognizer.process_event_at(&key_event(Key::D, true), start + Duration::from_millis(10));
        recognizer.process_event_at(&key_event(Key::D, false), start + Duration::from_millis(20));

        assert!(recognizer
            .drain_expired_at(start + Duration::from_millis(300))
            .is_empty());
    }

    #[test]
    fn tap_alone_continuous_irrelevant_events_do_not_starve_expired_pending_release() {
        let start = Instant::now();
        let mut recognizer = TapAloneRecognizer::new();
        let id = recognizer
            .register(tap_alone(TriggerKey::Key(Key::D)))
            .unwrap();

        recognizer.process_event_at(&key_event(Key::D, true), start);
        recognizer.process_event_at(&key_event(Key::D, false), start + Duration::from_millis(20));

        assert_eq!(
            recognizer
                .process_event_at(&key_event(Key::F, true), start + Duration::from_millis(300)),
            vec![TapAloneEvent { id }]
        );
    }

    #[test]
    fn tap_alone_rejects_duplicate_ids_and_triggers() {
        let mut recognizer = TapAloneRecognizer::new();
        recognizer
            .register_with_id(TapAloneId::from_u32(7), tap_alone(TriggerKey::Key(Key::D)))
            .unwrap();

        assert!(matches!(
            recognizer
                .register_with_id(TapAloneId::from_u32(7), tap_alone(TriggerKey::Key(Key::F))),
            Err(Error::TapAloneAlreadyRegistered(_))
        ));
        assert!(matches!(
            recognizer.register(tap_alone(TriggerKey::Key(Key::D))),
            Err(Error::TapAloneAlreadyRegistered(_))
        ));
    }

    #[test]
    fn tap_alone_explicit_id_advances_next_generated_id() {
        let mut recognizer = TapAloneRecognizer::new();
        recognizer
            .register_with_id(TapAloneId::from_u32(40), tap_alone(TriggerKey::Key(Key::D)))
            .unwrap();
        let id = recognizer
            .register(tap_alone(TriggerKey::Key(Key::F)))
            .unwrap();
        assert_eq!(id.as_u32(), 41);
    }

    #[test]
    fn tap_alone_unregister_clears_pending_release() {
        let start = Instant::now();
        let mut recognizer = TapAloneRecognizer::new();
        let id = recognizer
            .register(tap_alone(TriggerKey::Key(Key::D)))
            .unwrap();
        recognizer.process_event_at(&key_event(Key::D, true), start);
        recognizer.process_event_at(&key_event(Key::D, false), start + Duration::from_millis(20));

        recognizer.unregister(id).unwrap();
        assert!(recognizer
            .drain_expired_at(start + Duration::from_millis(300))
            .is_empty());
    }

    #[test]
    fn tap_alone_unregister_clears_active_and_suppressed_state() {
        let start = Instant::now();
        let mut recognizer = TapAloneRecognizer::new();
        let id = recognizer
            .register(tap_alone(TriggerKey::Key(Key::D)))
            .unwrap();
        recognizer.process_event_at(&key_event(Key::D, true), start);
        recognizer.unregister(id).unwrap();
        recognizer.process_event_at(&key_event(Key::D, false), start + Duration::from_millis(20));
        assert!(recognizer
            .drain_expired_at(start + Duration::from_millis(300))
            .is_empty());

        let id = recognizer
            .register(tap_alone(TriggerKey::Key(Key::D)))
            .unwrap();
        recognizer.process_event_at(&key_event(Key::D, true), start + Duration::from_millis(400));
        recognizer.process_event_at(
            &key_event(Key::D, false),
            start + Duration::from_millis(420),
        );
        assert_eq!(
            recognizer.drain_expired_at(start + Duration::from_millis(670)),
            vec![TapAloneEvent { id }]
        );
    }
}
