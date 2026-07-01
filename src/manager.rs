//! Platform-agnostic hotkey manager built on top of KeyboardListener

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use crate::listener::{BlockingHotkeys, KeyboardListener};
use crate::tap_alone::TapAloneRecognizer;
use crate::tap_pattern::TapPatternRecognizer;
use crate::types::{
    HandyKeysEvent, Hotkey, HotkeyEvent, HotkeyId, HotkeyState, KeyEvent, TapAlone, TapAloneEvent,
    TapAloneId, TapPattern, TapPatternEvent, TapPatternId,
};

const RECV_TIMEOUT: Duration = Duration::from_millis(100);
const EVENT_BUFFER_LIMIT: usize = 1024;

/// Internal state shared between the manager and the processing thread
struct ManagerState {
    hotkeys: HashMap<HotkeyId, Hotkey>,
    next_id: u32,
    /// Track which hotkeys are currently pressed
    pressed_hotkeys: HashSet<HotkeyId>,
    tap_patterns: TapPatternRecognizer,
    tap_alones: TapAloneRecognizer,
}

struct ProcessedEvents {
    tap_alone_events: Vec<TapAloneEvent>,
    hotkey_events: Vec<HotkeyEvent>,
    tap_events: Vec<TapPatternEvent>,
}

impl ManagerState {
    fn new() -> Self {
        Self {
            hotkeys: HashMap::new(),
            next_id: 0,
            pressed_hotkeys: HashSet::new(),
            tap_patterns: TapPatternRecognizer::new(),
            tap_alones: TapAloneRecognizer::new(),
        }
    }

    /// Process a key event and return any matching hotkey events
    fn process_event(&mut self, event: &KeyEvent) -> Vec<HotkeyEvent> {
        if event.activity.is_some() {
            return Vec::new();
        }

        let mut results = Vec::new();

        if event.is_key_down {
            // Check for hotkeys that should be pressed
            let to_press: Vec<HotkeyId> = self
                .hotkeys
                .iter()
                .filter(|(&id, hotkey)| {
                    hotkey.modifiers.matches(event.modifiers)
                        && hotkey.key == event.key
                        && !self.pressed_hotkeys.contains(&id)
                })
                .map(|(&id, _)| id)
                .collect();

            for id in to_press {
                self.pressed_hotkeys.insert(id);
                results.push(HotkeyEvent {
                    id,
                    state: HotkeyState::Pressed,
                });
            }
        } else {
            // Check for hotkeys that should be released
            // A hotkey is released when its key is released, or — for modifier
            // events — when the modifiers no longer match. A modifier event
            // (key == None) whose modifiers still match must not release a
            // modifier-only hotkey (e.g. tapping Shift while a Cmd-only hotkey
            // is held).
            let to_release: Vec<HotkeyId> = self
                .hotkeys
                .iter()
                .filter(|(&id, hotkey)| {
                    self.pressed_hotkeys.contains(&id)
                        && ((event.key.is_some() && hotkey.key == event.key)
                            || (event.key.is_none() && !hotkey.modifiers.matches(event.modifiers)))
                })
                .map(|(&id, _)| id)
                .collect();

            for id in to_release {
                self.pressed_hotkeys.remove(&id);
                results.push(HotkeyEvent {
                    id,
                    state: HotkeyState::Released,
                });
            }
        }

        results
    }

    fn process_all_events(&mut self, event: &KeyEvent, now: Instant) -> ProcessedEvents {
        let tap_alone_events = self.tap_alones.process_event_at(event, now);
        let hotkey_events = self.process_event(event);
        let tap_events = self.tap_patterns.process_event_at(event, now);
        ProcessedEvents {
            tap_alone_events,
            hotkey_events,
            tap_events,
        }
    }
}

/// Platform-agnostic Hotkey Manager
///
/// This manager wraps a `KeyboardListener` and filters events against
/// registered hotkeys, emitting `HotkeyEvent`s when matches occur.
///
/// Registered hotkeys are blocked from reaching other applications.
/// Note: On Linux, blocking requires write access to `/dev/uinput`
/// (non-blocked keystrokes are re-injected through it).
pub struct HotkeyManager {
    state: Arc<Mutex<ManagerState>>,
    event_receiver: Receiver<HandyKeysEvent>,
    event_buffer: Mutex<VecDeque<HandyKeysEvent>>,
    _thread_handle: Option<JoinHandle<()>>,
    running: Arc<std::sync::atomic::AtomicBool>,
    /// Shared set of hotkeys to block
    blocking_hotkeys: Option<BlockingHotkeys>,
}

impl HotkeyManager {
    /// Create a new HotkeyManager (non-blocking mode)
    ///
    /// On macOS, this will check for accessibility permissions and fail if not granted.
    pub fn new() -> Result<Self> {
        let listener = KeyboardListener::new()?;

        let (event_tx, event_rx) = mpsc::channel();
        let state = Arc::new(Mutex::new(ManagerState::new()));
        let running = Arc::new(std::sync::atomic::AtomicBool::new(true));

        let thread_state = Arc::clone(&state);
        let thread_running = Arc::clone(&running);

        let handle = thread::spawn(move || {
            Self::event_loop(listener, thread_state, event_tx, thread_running);
        });

        Ok(Self {
            state,
            event_receiver: event_rx,
            event_buffer: Mutex::new(VecDeque::new()),
            _thread_handle: Some(handle),
            running,
            blocking_hotkeys: None,
        })
    }

    /// Create a new HotkeyManager with blocking support
    ///
    /// On macOS, this will check for accessibility permissions and fail if not granted.
    /// Registered hotkeys will be blocked from reaching other applications.
    ///
    /// Note: On Linux, blocking requires write access to `/dev/uinput`
    /// and this fails with an actionable error without it.
    pub fn new_with_blocking() -> Result<Self> {
        let blocking_hotkeys: BlockingHotkeys = Arc::new(Mutex::new(HashSet::new()));
        let listener = KeyboardListener::new_with_blocking(blocking_hotkeys.clone())?;

        let (event_tx, event_rx) = mpsc::channel();
        let state = Arc::new(Mutex::new(ManagerState::new()));
        let running = Arc::new(std::sync::atomic::AtomicBool::new(true));

        let thread_state = Arc::clone(&state);
        let thread_running = Arc::clone(&running);

        let handle = thread::spawn(move || {
            Self::event_loop(listener, thread_state, event_tx, thread_running);
        });

        Ok(Self {
            state,
            event_receiver: event_rx,
            event_buffer: Mutex::new(VecDeque::new()),
            _thread_handle: Some(handle),
            running,
            blocking_hotkeys: Some(blocking_hotkeys),
        })
    }

    /// Event processing loop
    fn event_loop(
        listener: KeyboardListener,
        state: Arc<Mutex<ManagerState>>,
        sender: Sender<HandyKeysEvent>,
        running: Arc<std::sync::atomic::AtomicBool>,
    ) {
        while running.load(std::sync::atomic::Ordering::SeqCst) {
            let timeout = if let Ok(mut state) = state.lock() {
                let now = Instant::now();
                if !drain_expired_tap_alones(&mut state, &sender, now) {
                    return;
                }
                recv_timeout_for_state(&state, now)
            } else {
                RECV_TIMEOUT
            };

            // Block until we receive an event or timeout (to check running flag)
            match listener.recv_timeout(timeout) {
                Ok(key_event) => {
                    if let Ok(mut state) = state.lock() {
                        let events = state.process_all_events(&key_event, Instant::now());
                        if !send_processed_events(events, &sender) {
                            return;
                        }
                    }
                }
                Err(crate::error::Error::Timeout) => {
                    if let Ok(mut state) = state.lock() {
                        if !drain_expired_tap_alones(&mut state, &sender, Instant::now()) {
                            return;
                        }
                    }
                }
                Err(_) => {
                    // Listener disconnected, exit
                    return;
                }
            }
        }
    }

    /// Register a hotkey and return its unique ID
    ///
    /// Returns an error if the hotkey is already registered.
    pub fn register(&self, hotkey: Hotkey) -> Result<HotkeyId> {
        let mut state = self.state.lock().map_err(|_| Error::MutexPoisoned)?;

        // Check if already registered
        for (id, existing) in &state.hotkeys {
            if existing == &hotkey {
                return Err(Error::HotkeyAlreadyRegistered(format!(
                    "{} (id: {:?})",
                    hotkey, id
                )));
            }
        }

        let id = HotkeyId(state.next_id);
        state.next_id += 1;
        state.hotkeys.insert(id, hotkey);

        // Add to blocking set
        if let Some(blocking_hotkeys) = &self.blocking_hotkeys {
            if let Ok(mut blocking) = blocking_hotkeys.lock() {
                blocking.insert(hotkey);
            }
        }

        Ok(id)
    }

    /// Unregister a hotkey by its ID
    ///
    /// Returns an error if the hotkey ID is not found.
    pub fn unregister(&self, id: HotkeyId) -> Result<()> {
        let mut state = self.state.lock().map_err(|_| Error::MutexPoisoned)?;

        let hotkey = state.hotkeys.remove(&id);
        if hotkey.is_none() {
            return Err(Error::HotkeyNotFound(id));
        }

        // Remove from blocking set
        if let Some(blocking_hotkeys) = &self.blocking_hotkeys {
            if let Some(hotkey) = hotkey {
                if let Ok(mut blocking) = blocking_hotkeys.lock() {
                    blocking.remove(&hotkey);
                }
            }
        }

        Ok(())
    }

    /// Get the hotkey definition associated with an ID
    ///
    /// Returns `None` if the ID is not found.
    pub fn get_hotkey(&self, id: HotkeyId) -> Option<Hotkey> {
        let state = self.state.lock().ok()?;
        state.hotkeys.get(&id).copied()
    }

    /// Register a tap pattern and return its unique ID.
    pub fn register_tap_pattern(&self, pattern: TapPattern) -> Result<TapPatternId> {
        let mut state = self.state.lock().map_err(|_| Error::MutexPoisoned)?;
        state.tap_patterns.register(pattern)
    }

    /// Unregister a tap pattern by its ID.
    pub fn unregister_tap_pattern(&self, id: TapPatternId) -> Result<()> {
        let mut state = self.state.lock().map_err(|_| Error::MutexPoisoned)?;
        state.tap_patterns.unregister(id)
    }

    /// Get the tap pattern definition associated with an ID.
    pub fn get_tap_pattern(&self, id: TapPatternId) -> Option<TapPattern> {
        let state = self.state.lock().ok()?;
        state.tap_patterns.get(id)
    }

    /// Register a tap-alone trigger and return its unique ID.
    pub fn register_tap_alone(&self, pattern: TapAlone) -> Result<TapAloneId> {
        let mut state = self.state.lock().map_err(|_| Error::MutexPoisoned)?;
        state.tap_alones.register(pattern)
    }

    /// Unregister a tap-alone trigger by its ID.
    pub fn unregister_tap_alone(&self, id: TapAloneId) -> Result<()> {
        let mut state = self.state.lock().map_err(|_| Error::MutexPoisoned)?;
        state.tap_alones.unregister(id)
    }

    /// Get the tap-alone definition associated with an ID.
    pub fn get_tap_alone(&self, id: TapAloneId) -> Option<TapAlone> {
        let state = self.state.lock().ok()?;
        state.tap_alones.get(id)
    }

    /// Blocking receive for hotkey events.
    ///
    /// Blocks until a hotkey event is received or the event loop stops.
    /// Other event kinds observed while waiting are preserved in a bounded
    /// ordered buffer for other receive APIs. Use `try_recv_event` when every
    /// event kind must be consumed without filtering.
    pub fn recv(&self) -> Result<HotkeyEvent> {
        if let Some(event) = self.pop_buffered_hotkey() {
            return Ok(event);
        }
        loop {
            match self.recv_event()? {
                HandyKeysEvent::Hotkey(event) => return Ok(event),
                event => self.buffer_event(event),
            }
        }
    }

    /// Non-blocking receive for hotkey events.
    ///
    /// Returns `Some(event)` if an event is available, `None` otherwise.
    /// Other event kinds observed while searching are preserved in a bounded
    /// ordered buffer for other receive APIs.
    pub fn try_recv(&self) -> Option<HotkeyEvent> {
        if let Some(event) = self.pop_buffered_hotkey() {
            return Some(event);
        }
        loop {
            match self.try_recv_raw_event() {
                Some(HandyKeysEvent::Hotkey(event)) => return Some(event),
                Some(event) => self.buffer_event(event),
                None => return None,
            }
        }
    }

    /// Blocking receive for tap-pattern events.
    ///
    /// Other event kinds observed while waiting are preserved in a bounded
    /// ordered buffer for other receive APIs.
    pub fn recv_tap_pattern(&self) -> Result<TapPatternEvent> {
        if let Some(event) = self.pop_buffered_tap_pattern() {
            return Ok(event);
        }
        loop {
            match self.recv_event()? {
                HandyKeysEvent::TapPattern(event) => return Ok(event),
                event => self.buffer_event(event),
            }
        }
    }

    /// Non-blocking receive for tap-pattern events.
    ///
    /// Other event kinds observed while searching are preserved in a bounded
    /// ordered buffer for other receive APIs.
    pub fn try_recv_tap_pattern(&self) -> Option<TapPatternEvent> {
        if let Some(event) = self.pop_buffered_tap_pattern() {
            return Some(event);
        }
        loop {
            match self.try_recv_raw_event() {
                Some(HandyKeysEvent::TapPattern(event)) => return Some(event),
                Some(event) => self.buffer_event(event),
                None => return None,
            }
        }
    }

    /// Blocking receive for tap-alone events.
    ///
    /// Other event kinds observed while waiting are preserved in a bounded
    /// ordered buffer for other receive APIs.
    pub fn recv_tap_alone(&self) -> Result<TapAloneEvent> {
        if let Some(event) = self.pop_buffered_tap_alone() {
            return Ok(event);
        }
        loop {
            match self.recv_event()? {
                HandyKeysEvent::TapAlone(event) => return Ok(event),
                event => self.buffer_event(event),
            }
        }
    }

    /// Non-blocking receive for tap-alone events.
    ///
    /// Other event kinds observed while searching are preserved in a bounded
    /// ordered buffer for other receive APIs.
    pub fn try_recv_tap_alone(&self) -> Option<TapAloneEvent> {
        if let Some(event) = self.pop_buffered_tap_alone() {
            return Some(event);
        }
        loop {
            match self.try_recv_raw_event() {
                Some(HandyKeysEvent::TapAlone(event)) => return Some(event),
                Some(event) => self.buffer_event(event),
                None => return None,
            }
        }
    }

    /// Non-blocking receive for any HandyKeys event.
    ///
    /// Typed receivers remain available independently. This convenience method
    /// reads from the ordered multiplexed event queue used by native bridges.
    pub fn try_recv_event(&self) -> Option<HandyKeysEvent> {
        if let Some(event) = self.pop_buffered_event() {
            return Some(event);
        }
        match self.event_receiver.try_recv() {
            Ok(event) => Some(event),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => None,
        }
    }

    fn recv_event(&self) -> Result<HandyKeysEvent> {
        self.event_receiver
            .recv()
            .map_err(|_| Error::EventLoopNotRunning)
    }

    fn try_recv_raw_event(&self) -> Option<HandyKeysEvent> {
        match self.event_receiver.try_recv() {
            Ok(event) => Some(event),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => None,
        }
    }

    fn buffer_event(&self, event: HandyKeysEvent) {
        if let Ok(mut buffer) = self.event_buffer.lock() {
            if buffer.len() >= EVENT_BUFFER_LIMIT {
                buffer.pop_front();
            }
            buffer.push_back(event);
        }
    }

    fn pop_buffered_event(&self) -> Option<HandyKeysEvent> {
        self.event_buffer
            .lock()
            .ok()
            .and_then(|mut buffer| buffer.pop_front())
    }

    fn pop_buffered_hotkey(&self) -> Option<HotkeyEvent> {
        self.pop_buffered_matching(|event| match event {
            HandyKeysEvent::Hotkey(event) => Some(event),
            _ => None,
        })
    }

    fn pop_buffered_tap_pattern(&self) -> Option<TapPatternEvent> {
        self.pop_buffered_matching(|event| match event {
            HandyKeysEvent::TapPattern(event) => Some(event),
            _ => None,
        })
    }

    fn pop_buffered_tap_alone(&self) -> Option<TapAloneEvent> {
        self.pop_buffered_matching(|event| match event {
            HandyKeysEvent::TapAlone(event) => Some(event),
            _ => None,
        })
    }

    fn pop_buffered_matching<T>(&self, matcher: impl Fn(HandyKeysEvent) -> Option<T>) -> Option<T> {
        let mut buffer = self.event_buffer.lock().ok()?;
        let index = buffer.iter().position(|event| matcher(*event).is_some())?;
        buffer.remove(index).and_then(matcher)
    }

    /// Get the number of currently registered hotkeys
    pub fn hotkey_count(&self) -> usize {
        let state = if let Ok(s) = self.state.lock() {
            s
        } else {
            return 0;
        };
        state.hotkeys.len()
    }

    /// Get the number of currently registered tap patterns
    pub fn tap_pattern_count(&self) -> usize {
        let state = if let Ok(s) = self.state.lock() {
            s
        } else {
            return 0;
        };
        state.tap_patterns.count()
    }

    /// Get the number of currently registered tap-alone triggers
    pub fn tap_alone_count(&self) -> usize {
        let state = if let Ok(s) = self.state.lock() {
            s
        } else {
            return 0;
        };
        state.tap_alones.count()
    }
}

fn recv_timeout_for_state(state: &ManagerState, now: Instant) -> Duration {
    state
        .tap_alones
        .next_deadline_at()
        .map(|deadline| deadline.saturating_duration_since(now))
        .unwrap_or(RECV_TIMEOUT)
        .min(RECV_TIMEOUT)
}

fn drain_expired_tap_alones(
    state: &mut ManagerState,
    sender: &Sender<HandyKeysEvent>,
    now: Instant,
) -> bool {
    send_tap_alone_events(state.tap_alones.drain_expired_at(now), sender)
}

fn send_processed_events(events: ProcessedEvents, sender: &Sender<HandyKeysEvent>) -> bool {
    if !send_tap_alone_events(events.tap_alone_events, sender) {
        return false;
    }
    for event in events.hotkey_events {
        if sender.send(HandyKeysEvent::Hotkey(event)).is_err() {
            return false;
        }
    }
    for event in events.tap_events {
        if sender.send(HandyKeysEvent::TapPattern(event)).is_err() {
            return false;
        }
    }
    true
}

fn send_tap_alone_events(events: Vec<TapAloneEvent>, sender: &Sender<HandyKeysEvent>) -> bool {
    for event in events {
        if sender.send(HandyKeysEvent::TapAlone(event)).is_err() {
            return false;
        }
    }
    true
}

impl Drop for HotkeyManager {
    fn drop(&mut self) {
        self.running
            .store(false, std::sync::atomic::Ordering::SeqCst);
        // Join the thread to ensure clean shutdown
        if let Some(handle) = self._thread_handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        InputActivity, Key, Modifiers, MouseButtonActivity, TapAlone, TapPattern, TriggerKey,
    };
    use std::time::{Duration, Instant};

    fn make_key_event(modifiers: Modifiers, key: Option<Key>, is_key_down: bool) -> KeyEvent {
        KeyEvent {
            modifiers,
            key,
            is_key_down,
            changed_modifier: None,
            activity: None,
        }
    }

    fn make_modifier_event(
        modifiers: Modifiers,
        is_key_down: bool,
        changed: Modifiers,
    ) -> KeyEvent {
        KeyEvent {
            modifiers,
            key: None,
            is_key_down,
            changed_modifier: Some(changed),
            activity: None,
        }
    }

    fn make_activity_event(modifiers: Modifiers, activity: InputActivity) -> KeyEvent {
        KeyEvent {
            modifiers,
            key: None,
            is_key_down: true,
            changed_modifier: None,
            activity: Some(activity),
        }
    }

    mod manager_state {
        use super::*;

        #[test]
        fn register_and_lookup_hotkey() {
            let mut state = ManagerState::new();
            let hotkey = Hotkey::new(Modifiers::CMD, Key::K).unwrap();

            let id = HotkeyId(state.next_id);
            state.next_id += 1;
            state.hotkeys.insert(id, hotkey);

            assert_eq!(state.hotkeys.get(&id), Some(&hotkey));
            assert_eq!(state.hotkeys.len(), 1);
        }

        #[test]
        fn hotkey_press_generates_event() {
            let mut state = ManagerState::new();
            let hotkey = Hotkey::new(Modifiers::CMD, Key::K).unwrap();
            let id = HotkeyId(0);
            state.hotkeys.insert(id, hotkey);

            // Simulate Cmd+K key down (event uses side-specific modifier)
            let event = make_key_event(Modifiers::CMD_LEFT, Some(Key::K), true);
            let results = state.process_event(&event);

            assert_eq!(results.len(), 1);
            assert_eq!(results[0].id, id);
            assert_eq!(results[0].state, HotkeyState::Pressed);
            assert!(state.pressed_hotkeys.contains(&id));
        }

        #[test]
        fn hotkey_release_generates_event() {
            let mut state = ManagerState::new();
            let hotkey = Hotkey::new(Modifiers::CMD, Key::K).unwrap();
            let id = HotkeyId(0);
            state.hotkeys.insert(id, hotkey);

            // Press first
            let event = make_key_event(Modifiers::CMD_LEFT, Some(Key::K), true);
            state.process_event(&event);

            // Then release the key
            let event = make_key_event(Modifiers::CMD_LEFT, Some(Key::K), false);
            let results = state.process_event(&event);

            assert_eq!(results.len(), 1);
            assert_eq!(results[0].id, id);
            assert_eq!(results[0].state, HotkeyState::Released);
            assert!(!state.pressed_hotkeys.contains(&id));
        }

        #[test]
        fn no_duplicate_press_events() {
            let mut state = ManagerState::new();
            let hotkey = Hotkey::new(Modifiers::CMD, Key::K).unwrap();
            let id = HotkeyId(0);
            state.hotkeys.insert(id, hotkey);

            // Press once
            let event = make_key_event(Modifiers::CMD_LEFT, Some(Key::K), true);
            let results = state.process_event(&event);
            assert_eq!(results.len(), 1);

            // Press again (key repeat) - should not generate another event
            let results = state.process_event(&event);
            assert_eq!(results.len(), 0);
        }

        #[test]
        fn modifier_release_triggers_hotkey_release() {
            let mut state = ManagerState::new();
            let hotkey = Hotkey::new(Modifiers::CMD, Key::K).unwrap();
            let id = HotkeyId(0);
            state.hotkeys.insert(id, hotkey);

            // Press Cmd+K
            let event = make_key_event(Modifiers::CMD_LEFT, Some(Key::K), true);
            state.process_event(&event);
            assert!(state.pressed_hotkeys.contains(&id));

            // Release Cmd (while K is still held) - modifier event
            let event = make_modifier_event(Modifiers::empty(), false, Modifiers::CMD_LEFT);
            let results = state.process_event(&event);

            assert_eq!(results.len(), 1);
            assert_eq!(results[0].state, HotkeyState::Released);
            assert!(!state.pressed_hotkeys.contains(&id));
        }

        #[test]
        fn wrong_modifiers_dont_trigger() {
            let mut state = ManagerState::new();
            let hotkey = Hotkey::new(Modifiers::CMD, Key::K).unwrap();
            state.hotkeys.insert(HotkeyId(0), hotkey);

            // Press Shift+K instead of Cmd+K
            let event = make_key_event(Modifiers::SHIFT_LEFT, Some(Key::K), true);
            let results = state.process_event(&event);

            assert_eq!(results.len(), 0);
        }

        #[test]
        fn modifier_only_hotkey() {
            let mut state = ManagerState::new();
            let hotkey = Hotkey::new(Modifiers::CMD | Modifiers::SHIFT, None).unwrap();
            let id = HotkeyId(0);
            state.hotkeys.insert(id, hotkey);

            // Press Cmd+Shift (no key) — events use side-specific modifiers
            let event = make_modifier_event(
                Modifiers::CMD_LEFT | Modifiers::SHIFT_LEFT,
                true,
                Modifiers::SHIFT_LEFT,
            );
            let results = state.process_event(&event);

            assert_eq!(results.len(), 1);
            assert_eq!(results[0].state, HotkeyState::Pressed);
        }

        #[test]
        fn multiple_hotkeys_same_key() {
            let mut state = ManagerState::new();

            // Cmd+K and Ctrl+K
            let hotkey1 = Hotkey::new(Modifiers::CMD, Key::K).unwrap();
            let hotkey2 = Hotkey::new(Modifiers::CTRL, Key::K).unwrap();
            let id1 = HotkeyId(0);
            let id2 = HotkeyId(1);
            state.hotkeys.insert(id1, hotkey1);
            state.hotkeys.insert(id2, hotkey2);

            // Press Cmd+K
            let event = make_key_event(Modifiers::CMD_LEFT, Some(Key::K), true);
            let results = state.process_event(&event);

            assert_eq!(results.len(), 1);
            assert_eq!(results[0].id, id1);

            // Press Ctrl+K (release Cmd first)
            state.pressed_hotkeys.clear();
            let event = make_key_event(Modifiers::CTRL_LEFT, Some(Key::K), true);
            let results = state.process_event(&event);

            assert_eq!(results.len(), 1);
            assert_eq!(results[0].id, id2);
        }

        #[test]
        fn key_only_hotkey() {
            let mut state = ManagerState::new();
            let hotkey = Hotkey::new(Modifiers::empty(), Key::F1).unwrap();
            let id = HotkeyId(0);
            state.hotkeys.insert(id, hotkey);

            // Press F1 with no modifiers
            let event = make_key_event(Modifiers::empty(), Some(Key::F1), true);
            let results = state.process_event(&event);

            assert_eq!(results.len(), 1);
            assert_eq!(results[0].state, HotkeyState::Pressed);

            // F1 with modifiers should NOT trigger
            state.pressed_hotkeys.clear();
            let event = make_key_event(Modifiers::CMD_LEFT, Some(Key::F1), true);
            let results = state.process_event(&event);

            assert_eq!(results.len(), 0);
        }

        #[test]
        fn modifier_only_hotkey_not_released_by_unrelated_modifier() {
            let mut state = ManagerState::new();
            let hotkey = Hotkey::new(Modifiers::CMD, None).unwrap();
            let id = HotkeyId(0);
            state.hotkeys.insert(id, hotkey);

            // Cmd down — hotkey pressed
            let event = make_modifier_event(Modifiers::CMD_LEFT, true, Modifiers::CMD_LEFT);
            let results = state.process_event(&event);
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].state, HotkeyState::Pressed);

            // Shift down while Cmd held — no state change
            let event = make_modifier_event(
                Modifiers::CMD_LEFT | Modifiers::SHIFT_LEFT,
                true,
                Modifiers::SHIFT_LEFT,
            );
            assert_eq!(state.process_event(&event).len(), 0);

            // Shift up — Cmd is still held and still matches, so the hotkey
            // must NOT be released
            let event = make_modifier_event(Modifiers::CMD_LEFT, false, Modifiers::SHIFT_LEFT);
            assert_eq!(state.process_event(&event).len(), 0);
            assert!(state.pressed_hotkeys.contains(&id));

            // Cmd up — now it releases
            let event = make_modifier_event(Modifiers::empty(), false, Modifiers::CMD_LEFT);
            let results = state.process_event(&event);
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].state, HotkeyState::Released);
            assert!(!state.pressed_hotkeys.contains(&id));
        }

        #[test]
        fn modifier_only_hotkey_releases_on_own_modifier_release() {
            let mut state = ManagerState::new();
            let hotkey = Hotkey::new(Modifiers::CMD, None).unwrap();
            let id = HotkeyId(0);
            state.hotkeys.insert(id, hotkey);

            let event = make_modifier_event(Modifiers::CMD_LEFT, true, Modifiers::CMD_LEFT);
            state.process_event(&event);
            assert!(state.pressed_hotkeys.contains(&id));

            let event = make_modifier_event(Modifiers::empty(), false, Modifiers::CMD_LEFT);
            let results = state.process_event(&event);
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].state, HotkeyState::Released);
        }

        #[test]
        fn compound_modifier_only_hotkey_releases_on_partial_release() {
            let mut state = ManagerState::new();
            let hotkey = Hotkey::new(Modifiers::CMD | Modifiers::SHIFT, None).unwrap();
            let id = HotkeyId(0);
            state.hotkeys.insert(id, hotkey);

            // Cmd down, then Shift down — pressed once both are held
            let event = make_modifier_event(Modifiers::CMD_LEFT, true, Modifiers::CMD_LEFT);
            assert_eq!(state.process_event(&event).len(), 0);
            let event = make_modifier_event(
                Modifiers::CMD_LEFT | Modifiers::SHIFT_LEFT,
                true,
                Modifiers::SHIFT_LEFT,
            );
            let results = state.process_event(&event);
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].state, HotkeyState::Pressed);

            // Releasing either modifier breaks the match — released
            let event = make_modifier_event(Modifiers::SHIFT_LEFT, false, Modifiers::CMD_LEFT);
            let results = state.process_event(&event);
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].state, HotkeyState::Released);
        }

        #[test]
        fn keyed_hotkey_not_released_by_unrelated_key_release() {
            let mut state = ManagerState::new();
            let hotkey = Hotkey::new(Modifiers::CMD, Key::K).unwrap();
            let id = HotkeyId(0);
            state.hotkeys.insert(id, hotkey);

            let event = make_key_event(Modifiers::CMD_LEFT, Some(Key::K), true);
            state.process_event(&event);
            assert!(state.pressed_hotkeys.contains(&id));

            // Release of a different key while Cmd+K is held — no release
            let event = make_key_event(Modifiers::CMD_LEFT, Some(Key::J), false);
            assert_eq!(state.process_event(&event).len(), 0);
            assert!(state.pressed_hotkeys.contains(&id));
        }

        #[test]
        fn side_specific_hotkey_matches_correct_side() {
            let mut state = ManagerState::new();
            // Register CtrlRight+Space
            let hotkey = Hotkey::new(Modifiers::CTRL_RIGHT, Key::Space).unwrap();
            let id = HotkeyId(0);
            state.hotkeys.insert(id, hotkey);

            // Left ctrl should not trigger
            let event = make_key_event(Modifiers::CTRL_LEFT, Some(Key::Space), true);
            assert_eq!(state.process_event(&event).len(), 0);

            // Right ctrl should trigger
            let event = make_key_event(Modifiers::CTRL_RIGHT, Some(Key::Space), true);
            let results = state.process_event(&event);
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].state, HotkeyState::Pressed);
        }

        #[test]
        fn compound_hotkey_matches_either_side() {
            let mut state = ManagerState::new();
            let hotkey = Hotkey::new(Modifiers::CMD, Key::K).unwrap();
            let id = HotkeyId(0);
            state.hotkeys.insert(id, hotkey);

            // Left Cmd triggers
            let event = make_key_event(Modifiers::CMD_LEFT, Some(Key::K), true);
            let results = state.process_event(&event);
            assert_eq!(results.len(), 1);

            // Release
            state.pressed_hotkeys.clear();

            // Right Cmd also triggers
            let event = make_key_event(Modifiers::CMD_RIGHT, Some(Key::K), true);
            let results = state.process_event(&event);
            assert_eq!(results.len(), 1);
        }

        #[test]
        fn tap_pattern_registration_lookup_and_unregister() {
            let mut state = ManagerState::new();
            let pattern = TapPattern::double_tap(
                TriggerKey::Modifier(Modifiers::SHIFT_LEFT),
                Duration::from_millis(250),
            )
            .unwrap();

            let id = state.tap_patterns.register(pattern).unwrap();
            assert_eq!(state.tap_patterns.get(id), Some(pattern));
            assert_eq!(state.tap_patterns.count(), 1);

            state.tap_patterns.unregister(id).unwrap();
            assert_eq!(state.tap_patterns.count(), 0);
        }

        #[test]
        fn process_all_events_includes_tap_pattern_events() {
            let mut state = ManagerState::new();
            let pattern =
                TapPattern::double_tap(TriggerKey::Key(Key::D), Duration::from_millis(250))
                    .unwrap();
            let id = state.tap_patterns.register(pattern).unwrap();
            let start = Instant::now();

            let first_down = make_key_event(Modifiers::empty(), Some(Key::D), true);
            let first_up = make_key_event(Modifiers::empty(), Some(Key::D), false);
            let second_down = make_key_event(Modifiers::empty(), Some(Key::D), true);

            assert!(state
                .process_all_events(&first_down, start)
                .tap_events
                .is_empty());
            state.process_all_events(&first_up, start + Duration::from_millis(10));
            let events = state.process_all_events(&second_down, start + Duration::from_millis(50));

            assert_eq!(
                events.tap_events,
                vec![TapPatternEvent {
                    id,
                    tap_count: 2,
                    is_key_down: true,
                }]
            );
        }

        #[test]
        fn tap_alone_registration_lookup_and_unregister() {
            let mut state = ManagerState::new();
            let pattern = TapAlone::new(
                TriggerKey::Key(Key::D),
                Duration::from_millis(1000),
                Duration::from_millis(250),
            )
            .unwrap();

            let id = state.tap_alones.register(pattern).unwrap();
            assert_eq!(state.tap_alones.get(id), Some(pattern));
            assert_eq!(state.tap_alones.count(), 1);

            state.tap_alones.unregister(id).unwrap();
            assert_eq!(state.tap_alones.count(), 0);
        }

        #[test]
        fn process_all_events_includes_tap_alone_events_after_deadline() {
            let mut state = ManagerState::new();
            let id = state
                .tap_alones
                .register(
                    TapAlone::new(
                        TriggerKey::Key(Key::D),
                        Duration::from_millis(1000),
                        Duration::from_millis(250),
                    )
                    .unwrap(),
                )
                .unwrap();
            let start = Instant::now();

            state.process_all_events(
                &make_key_event(Modifiers::empty(), Some(Key::D), true),
                start,
            );
            state.process_all_events(
                &make_key_event(Modifiers::empty(), Some(Key::D), false),
                start + Duration::from_millis(40),
            );

            assert_eq!(
                state
                    .tap_alones
                    .drain_expired_at(start + Duration::from_millis(290)),
                vec![TapAloneEvent { id }]
            );
        }

        #[test]
        fn process_all_events_drains_expired_tap_alone_before_new_event() {
            let mut state = ManagerState::new();
            let id = state
                .tap_alones
                .register(
                    TapAlone::new(
                        TriggerKey::Key(Key::D),
                        Duration::from_millis(1000),
                        Duration::from_millis(250),
                    )
                    .unwrap(),
                )
                .unwrap();
            let start = Instant::now();

            state.process_all_events(
                &make_key_event(Modifiers::empty(), Some(Key::D), true),
                start,
            );
            state.process_all_events(
                &make_key_event(Modifiers::empty(), Some(Key::D), false),
                start + Duration::from_millis(20),
            );
            let events = state.process_all_events(
                &make_key_event(Modifiers::empty(), Some(Key::D), true),
                start + Duration::from_millis(300),
            );

            assert_eq!(events.tap_alone_events, vec![TapAloneEvent { id }]);
        }

        #[test]
        fn ordered_queue_sends_expired_tap_alone_before_current_hotkey() {
            let mut state = ManagerState::new();
            let hotkey_id = HotkeyId(7);
            state
                .hotkeys
                .insert(hotkey_id, Hotkey::new(Modifiers::empty(), Key::D).unwrap());
            let tap_alone_id = state
                .tap_alones
                .register(
                    TapAlone::new(
                        TriggerKey::Key(Key::D),
                        Duration::from_millis(1000),
                        Duration::from_millis(250),
                    )
                    .unwrap(),
                )
                .unwrap();
            let start = Instant::now();

            state.process_all_events(
                &make_key_event(Modifiers::empty(), Some(Key::D), true),
                start,
            );
            state.process_all_events(
                &make_key_event(Modifiers::empty(), Some(Key::D), false),
                start + Duration::from_millis(20),
            );
            let events = state.process_all_events(
                &make_key_event(Modifiers::empty(), Some(Key::D), true),
                start + Duration::from_millis(300),
            );

            let (event_tx, event_rx) = mpsc::channel();
            assert!(send_processed_events(events, &event_tx));

            assert_eq!(
                event_rx.try_recv().unwrap(),
                HandyKeysEvent::TapAlone(TapAloneEvent { id: tap_alone_id })
            );
            assert_eq!(
                event_rx.try_recv().unwrap(),
                HandyKeysEvent::Hotkey(HotkeyEvent {
                    id: hotkey_id,
                    state: HotkeyState::Pressed,
                })
            );
        }

        #[test]
        fn internal_activity_does_not_trigger_modifier_only_hotkeys() {
            let mut state = ManagerState::new();
            let hotkey = Hotkey::new(Modifiers::CTRL_LEFT, None).unwrap();
            let id = HotkeyId(0);
            state.hotkeys.insert(id, hotkey);

            let events = state.process_all_events(
                &make_activity_event(
                    Modifiers::CTRL_LEFT,
                    InputActivity::MouseButtonDown(MouseButtonActivity::Left),
                ),
                Instant::now(),
            );

            assert!(events.hotkey_events.is_empty());
        }

        #[test]
        fn tap_alone_deadline_timeout_uses_min_deadline_and_recv_timeout() {
            let start = Instant::now();
            let mut state = ManagerState::new();
            state
                .tap_alones
                .register(
                    TapAlone::new(
                        TriggerKey::Key(Key::D),
                        Duration::from_millis(1000),
                        Duration::from_millis(50),
                    )
                    .unwrap(),
                )
                .unwrap();
            state.process_all_events(
                &make_key_event(Modifiers::empty(), Some(Key::D), true),
                start,
            );
            state.process_all_events(
                &make_key_event(Modifiers::empty(), Some(Key::D), false),
                start,
            );
            assert_eq!(
                recv_timeout_for_state(&state, start),
                Duration::from_millis(50)
            );

            let mut later = ManagerState::new();
            later
                .tap_alones
                .register(
                    TapAlone::new(
                        TriggerKey::Key(Key::D),
                        Duration::from_millis(1000),
                        Duration::from_millis(250),
                    )
                    .unwrap(),
                )
                .unwrap();
            later.process_all_events(
                &make_key_event(Modifiers::empty(), Some(Key::D), true),
                start,
            );
            later.process_all_events(
                &make_key_event(Modifiers::empty(), Some(Key::D), false),
                start,
            );
            assert_eq!(recv_timeout_for_state(&later, start), RECV_TIMEOUT);
        }
    }

    #[test]
    fn typed_receive_buffers_unmatched_events_for_generic_receive() {
        let (event_tx, event_rx) = mpsc::channel();
        let manager = HotkeyManager {
            state: Arc::new(Mutex::new(ManagerState::new())),
            event_receiver: event_rx,
            event_buffer: Mutex::new(VecDeque::new()),
            _thread_handle: None,
            running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            blocking_hotkeys: None,
        };
        let hotkey_event = HotkeyEvent {
            id: HotkeyId(3),
            state: HotkeyState::Pressed,
        };
        let tap_alone_event = TapAloneEvent {
            id: TapAloneId::from_u32(9),
        };

        event_tx.send(HandyKeysEvent::Hotkey(hotkey_event)).unwrap();
        event_tx
            .send(HandyKeysEvent::TapAlone(tap_alone_event))
            .unwrap();

        assert_eq!(manager.try_recv_tap_alone(), Some(tap_alone_event));
        assert_eq!(
            manager.try_recv_event(),
            Some(HandyKeysEvent::Hotkey(hotkey_event))
        );
        assert_eq!(manager.try_recv_event(), None);
    }

    #[test]
    fn typed_receive_unmatched_event_buffer_is_bounded() {
        let (event_tx, event_rx) = mpsc::channel();
        let manager = HotkeyManager {
            state: Arc::new(Mutex::new(ManagerState::new())),
            event_receiver: event_rx,
            event_buffer: Mutex::new(VecDeque::new()),
            _thread_handle: None,
            running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            blocking_hotkeys: None,
        };

        for offset in 0..(EVENT_BUFFER_LIMIT + 2) {
            event_tx
                .send(HandyKeysEvent::TapAlone(TapAloneEvent {
                    id: TapAloneId::from_u32(offset as u32),
                }))
                .unwrap();
        }

        assert_eq!(manager.try_recv(), None);
        assert_eq!(
            manager.event_buffer.lock().unwrap().len(),
            EVENT_BUFFER_LIMIT
        );
    }
}
