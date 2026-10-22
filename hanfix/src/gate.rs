//! Ordering gate between the low-level hooks and the worker.
//!
//! The one rule: an input may pass straight through only when nothing is held and every event
//! we injected has come back through the hook. Once anything is held, everything after it is held
//! too, so the application always receives input in the order it physically happened.

use std::collections::VecDeque;

pub const VK_SHIFT: u16 = 0x10;
pub const VK_CONTROL: u16 = 0x11;
pub const VK_MENU: u16 = 0x12;
pub const VK_SPACE: u16 = 0x20;
pub const VK_HANGUL: u16 = 0x15;
pub const VK_LSHIFT: u16 = 0xA0;
pub const VK_RSHIFT: u16 = 0xA1;
pub const VK_LCONTROL: u16 = 0xA2;
pub const VK_RCONTROL: u16 = 0xA3;
pub const VK_LMENU: u16 = 0xA4;
pub const VK_RMENU: u16 = 0xA5;
pub const VK_LWIN: u16 = 0x5B;
pub const VK_RWIN: u16 = 0x5C;

/// How long injected events may stay unechoed before we assume they were lost.
pub const WATCHDOG_MS: u64 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    pub vk: u16,
    pub scan: u16,
    pub extended: bool,
    pub up: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseButton {
    /// `MOUSEEVENTF_*` button flags for re-injection.
    pub flags: u32,
    pub data: u32,
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Key(Key),
    Mouse(MouseButton),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    /// Already delivered to the application; the worker only observes it.
    Passed(Input),
    /// Swallowed; the worker must inject it (possibly after a correction).
    Held(Input),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Pass,
    Swallow,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub win: bool,
}

impl Mods {
    /// Update from a key event; returns true if the key was a modifier.
    pub fn apply(&mut self, k: &Key) -> bool {
        let down = !k.up;
        match k.vk {
            VK_SHIFT | VK_LSHIFT | VK_RSHIFT => self.shift = down,
            VK_CONTROL | VK_LCONTROL | VK_RCONTROL => self.ctrl = down,
            VK_MENU | VK_LMENU | VK_RMENU => self.alt = down,
            VK_LWIN | VK_RWIN => self.win = down,
            _ => return false,
        }
        true
    }

    pub fn any(&self) -> bool {
        self.shift || self.ctrl || self.alt || self.win
    }
}

pub fn is_letter(vk: u16) -> bool {
    (0x41..=0x5A).contains(&vk)
}

#[derive(Debug, Default)]
pub struct Gate {
    stream: VecDeque<Item>,
    held: usize,
    in_flight: u32,
    epoch: u32,
    last_activity_ms: u64,
    letters: u32,
    phys: Mods,
    /// Between `commit` and `sent`: the injected events are not in the system queue yet.
    sending: bool,
    /// Set by the worker while it verifies the IME mode after a correction: keeps later input held.
    blocked: bool,
    /// While non-zero and in the future, a 한/영 press right after a conversion is held as "undo".
    pub undo_until_ms: u64,
    /// The word in progress was converted while typing: a 한/영 press before it ends is held as undo.
    pub live_word: bool,
    /// The 한/영 key-down was held as undo: hold its key-up too so the app never sees an unpaired up.
    hold_hangul_up: bool,
    /// Set when the worker stopped responding: everything passes straight through until it recovers.
    failed: bool,
    /// Bumped whenever the queue is thrown away (fail-open), so a decision made on an item peeked
    /// before that can be recognised as stale at commit time.
    generation: u64,
    /// When false nothing is held for correction (paused, elevated window, excluded app).
    pub enabled: bool,
    pub watchdog_resets: u32,
}

impl Gate {
    pub fn new() -> Gate {
        Gate { enabled: true, ..Gate::default() }
    }

    pub fn is_passing(&self) -> bool {
        self.held == 0 && self.in_flight == 0 && !self.blocked
    }

    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    pub fn in_flight(&self) -> u32 {
        self.in_flight
    }

    pub fn queued(&self) -> usize {
        self.stream.len()
    }

    fn hold(&mut self, input: Input) -> Decision {
        self.stream.push_back(Item::Held(input));
        self.held += 1;
        Decision::Swallow
    }

    fn pass(&mut self, input: Input) -> Decision {
        self.stream.push_back(Item::Passed(input));
        Decision::Pass
    }

    /// Hook thread: record that the hook is alive and the system input queue is moving.
    pub fn touch(&mut self, now_ms: u64) {
        self.last_activity_ms = now_ms;
    }

    /// Hook thread: a physical (or foreign-injected) key event arrived.
    pub fn on_key(&mut self, k: Key, now_ms: u64) -> Decision {
        self.last_activity_ms = now_ms;
        let is_mod = self.phys.apply(&k);
        if self.failed {
            return Decision::Pass;
        }
        if k.up && k.vk == VK_HANGUL && std::mem::take(&mut self.hold_hangul_up) {
            return self.hold(Input::Key(k));
        }
        let passing = self.is_passing();
        let undo = passing && !k.up && k.vk == VK_HANGUL && self.enabled && (now_ms < self.undo_until_ms || self.live_word);
        // Word bookkeeping runs on every path: letters typed while input is held still belong to
        // the word that the next Space must check, even if the gate has reopened by then.
        let boundary = !k.up && !is_mod && self.note_word_key(k.vk);
        if !k.up && !is_mod {
            self.undo_until_ms = 0;
        }
        if !passing {
            return self.hold(Input::Key(k));
        }
        if undo {
            self.hold_hangul_up = true;
            self.live_word = false;
            return self.hold(Input::Key(k));
        }
        if boundary {
            return self.hold(Input::Key(k));
        }
        self.pass(Input::Key(k))
    }

    /// Track whether a word is in progress. Returns true for a Space that ends a word and should be
    /// checked (enabled, no modifiers held — Shift+Space may be the 한/영 key).
    fn note_word_key(&mut self, vk: u16) -> bool {
        if vk == VK_SPACE {
            let check = self.enabled && !self.phys.any() && self.letters > 0;
            self.letters = 0;
            self.live_word = false;
            return check;
        }
        if is_letter(vk) && !self.phys.ctrl && !self.phys.alt && !self.phys.win {
            self.letters += 1;
        } else if !(0x30..=0x39).contains(&vk) && !(0xBA..=0xC0).contains(&vk) && !(0xDB..=0xDE).contains(&vk) {
            // Anything but letters, digits and punctuation ends the word.
            self.letters = 0;
            self.live_word = false;
        }
        false
    }

    /// Hook thread: a mouse button event arrived. Moves and wheel never come here.
    pub fn on_mouse_button(&mut self, m: MouseButton, now_ms: u64) -> Decision {
        self.last_activity_ms = now_ms;
        if self.failed {
            return Decision::Pass;
        }
        if !self.is_passing() {
            return self.hold(Input::Mouse(m));
        }
        self.letters = 0;
        self.undo_until_ms = 0;
        self.live_word = false;
        self.pass(Input::Mouse(m))
    }

    /// Hook thread: one of our injected events came back.
    pub fn on_echo(&mut self, epoch: u32, now_ms: u64) {
        if epoch == self.epoch {
            self.in_flight = self.in_flight.saturating_sub(1);
            self.last_activity_ms = now_ms;
        }
    }

    /// Worker: look at the next item. `Passed` items are removed; a `Held` item stays queued
    /// (still blocking pass-through) until [`Gate::commit`].
    pub fn next(&mut self) -> Option<Item> {
        match self.stream.front()? {
            Item::Passed(_) => self.stream.pop_front(),
            held @ Item::Held(_) => Some(*held),
        }
    }

    /// Worker: read together with [`Gate::next`] under the same lock; pass it back to `commit`.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Worker: atomically remove the front held item and account for the `n` events about to be
    /// injected for it. Returns the epoch to tag them with; call `SendInput` after releasing the lock.
    /// Returns `None` (inject nothing) if the queue was reset since the item was peeked.
    pub fn commit(&mut self, n: u32, now_ms: u64, peeked_generation: u64) -> Option<u32> {
        if peeked_generation != self.generation || !matches!(self.stream.front(), Some(Item::Held(_))) {
            return None;
        }
        self.stream.pop_front();
        self.held -= 1;
        self.in_flight += n;
        self.last_activity_ms = now_ms;
        self.sending = true;
        Some(self.epoch)
    }

    /// Worker: start a correction nobody asked for (mid-word conversion). From now on everything is
    /// held; the worker then drains the already-passed items, decides, and calls [`Gate::reserve`].
    pub fn begin_unsolicited(&mut self) -> u64 {
        self.blocked = true;
        self.generation
    }

    /// Worker: account for `n` events about to be injected while blocked (no held item consumed).
    pub fn reserve(&mut self, n: u32, now_ms: u64, generation: u64) -> Option<u32> {
        if generation != self.generation {
            return None;
        }
        self.in_flight += n;
        self.last_activity_ms = now_ms;
        self.sending = true;
        Some(self.epoch)
    }

    /// Worker: keep all later input held (true) while verifying the IME mode, then release (false).
    pub fn set_blocked(&mut self, blocked: bool) {
        self.blocked = blocked;
    }

    /// Worker: an injection attempt died half way; clear its transient flags so the gate cannot
    /// stay blocked (the watchdog then handles any echoes that never come).
    pub fn abort_injection(&mut self) {
        self.blocked = false;
        self.sending = false;
    }

    /// Worker: `SendInput` returned. `inserted` is its return value for the `n` committed events.
    pub fn sent(&mut self, epoch: u32, n: u32, inserted: u32, now_ms: u64) {
        self.sending = false;
        self.last_activity_ms = now_ms;
        if epoch == self.epoch {
            self.in_flight = self.in_flight.saturating_sub(n.saturating_sub(inserted));
        }
    }

    /// Worker: give up on echoes that never came. Returns true if a reset happened.
    ///
    /// Fires only after the hook saw *nothing at all* for `WATCHDOG_MS`: while anything is still in
    /// the system input queue the hook keeps being called, so a fully idle hook means the missing
    /// echoes were really lost. (A whole-system input stall longer than this is the one accepted
    /// degraded case; every reset is logged.)
    pub fn watchdog(&mut self, now_ms: u64) -> bool {
        // Never while a send is under way: those events have not even entered the system queue.
        if !self.sending && self.in_flight > 0 && now_ms.saturating_sub(self.last_activity_ms) > WATCHDOG_MS {
            self.epoch = self.epoch.wrapping_add(1);
            self.in_flight = 0;
            self.watchdog_resets += 1;
            return true;
        }
        false
    }

    /// Hook thread: the worker stopped (panic or stall) while input was held. Give the keyboard back:
    /// drop the queue and pass everything until the worker calls [`Gate::take_failed`].
    /// Held events are lost in this case; a dead keyboard would be worse.
    pub fn fail_open(&mut self) -> bool {
        if self.failed || self.is_passing() {
            return false;
        }
        self.failed = true;
        self.stream.clear();
        self.held = 0;
        self.in_flight = 0;
        self.blocked = false;
        self.sending = false;
        self.hold_hangul_up = false;
        self.epoch = self.epoch.wrapping_add(1);
        self.generation += 1;
        true
    }

    /// Worker: true once after a fail-open; resumes normal operation.
    pub fn take_failed(&mut self) -> bool {
        std::mem::take(&mut self.failed)
    }

    /// Forget the word in progress (focus change, pause).
    pub fn reset_word(&mut self) {
        self.letters = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Gate {
        fn on_key0(&mut self, k: Key) -> Decision {
            self.on_key(k, 0)
        }

        fn commit0(&mut self, n: u32, now_ms: u64) -> u32 {
            let generation = self.generation();
            self.commit(n, now_ms, generation).expect("held item at the front")
        }
    }

    fn key(vk: u16, up: bool) -> Key {
        Key { vk, scan: 0, extended: false, up }
    }

    #[test]
    fn letters_pass_space_after_letters_is_held() {
        let mut g = Gate::new();
        assert_eq!(g.on_key0(key(0x41, false)), Decision::Pass);
        assert_eq!(g.on_key0(key(0x41, true)), Decision::Pass);
        assert_eq!(g.on_key0(key(VK_SPACE, false)), Decision::Swallow);
        // Everything after is held, including the key-up of the held space.
        assert_eq!(g.on_key0(key(0x42, false)), Decision::Swallow);
        assert_eq!(g.on_key0(key(VK_SPACE, true)), Decision::Swallow);
    }

    #[test]
    fn space_without_word_or_with_modifier_passes() {
        let mut g = Gate::new();
        assert_eq!(g.on_key0(key(VK_SPACE, false)), Decision::Pass);
        g.on_key0(key(0x41, false));
        g.on_key0(key(VK_LSHIFT, false));
        assert_eq!(g.on_key0(key(VK_SPACE, false)), Decision::Pass, "Shift+Space may be the 한/영 key");
    }

    #[test]
    fn held_item_blocks_until_commit_and_echo() {
        let mut g = Gate::new();
        g.on_key0(key(0x41, false));
        g.on_key0(key(VK_SPACE, false));
        assert!(matches!(g.next(), Some(Item::Passed(_))));
        assert!(matches!(g.next(), Some(Item::Held(_))));
        assert!(!g.is_passing(), "peeking keeps it held");
        let e = g.commit0(3, 0);
        assert!(!g.is_passing(), "in flight");
        g.on_echo(e, 1);
        g.on_echo(e, 1);
        assert!(!g.is_passing());
        g.on_echo(e, 1);
        assert!(g.is_passing());
    }

    #[test]
    fn blocked_gate_holds_everything() {
        let mut g = Gate::new();
        g.set_blocked(true);
        assert_eq!(g.on_key0(key(0x41, false)), Decision::Swallow);
        assert!(matches!(g.next(), Some(Item::Held(_))));
        g.set_blocked(false);
        let e = g.commit0(1, 0);
        g.on_echo(e, 0);
        assert!(g.is_passing());
    }

    #[test]
    fn hangul_key_right_after_conversion_is_held_for_undo() {
        let mut g = Gate::new();
        g.undo_until_ms = 2000;
        assert_eq!(g.on_key(key(VK_HANGUL, false), 100), Decision::Swallow);
        let mut g = Gate::new();
        g.undo_until_ms = 2000;
        assert_eq!(g.on_key(key(VK_HANGUL, false), 2500), Decision::Pass, "window expired");
        let mut g = Gate::new();
        g.undo_until_ms = 2000;
        g.on_key(key(0x41, false), 100);
        assert_eq!(g.on_key(key(VK_HANGUL, false), 200), Decision::Pass, "another key came first");
    }

    /// Fast typing: the next word is typed while the previous correction is still being verified,
    /// and its Space arrives after the gate reopened. That Space must still be checked.
    #[test]
    fn letters_typed_while_held_still_count_for_next_space() {
        let mut g = Gate::new();
        g.on_key0(key(0x41, false));
        g.on_key0(key(VK_SPACE, false));
        g.next();
        g.next();
        let e = g.commit0(1, 0);
        g.set_blocked(true);
        g.sent(e, 1, 1, 0);
        g.on_echo(e, 0);
        assert_eq!(g.on_key0(key(0x42, false)), Decision::Swallow, "held while verifying");
        g.next();
        let e = g.commit0(1, 0);
        g.sent(e, 1, 1, 0);
        g.on_echo(e, 0);
        g.set_blocked(false);
        assert!(g.is_passing());
        assert_eq!(g.on_key0(key(VK_SPACE, false)), Decision::Swallow);
    }

    /// A worker that was slow (fail-open fired while it was deciding) must not inject a stale
    /// decision or crash; it resumes after `take_failed`.
    #[test]
    fn stale_decision_after_fail_open_is_refused() {
        let mut g = Gate::new();
        g.on_key0(key(0x41, false));
        g.on_key0(key(VK_SPACE, false));
        g.next();
        assert!(matches!(g.next(), Some(Item::Held(_))));
        let peeked = g.generation();
        assert!(g.fail_open());
        g.on_key0(key(0x42, false)); // passes while failed open
        assert_eq!(g.commit(2, 0, peeked), None);
        assert!(g.take_failed());
        g.on_key0(key(0x43, false));
        assert_eq!(g.on_key0(key(VK_SPACE, false)), Decision::Swallow, "normal operation resumed");
        while let Some(Item::Passed(_)) = g.next() {}
        let now_gen = g.generation();
        assert!(g.commit(1, 0, now_gen).is_some());
    }

    #[test]
    fn fail_open_releases_the_keyboard() {
        let mut g = Gate::new();
        g.on_key0(key(0x41, false));
        g.on_key0(key(VK_SPACE, false));
        assert_eq!(g.on_key0(key(0x42, false)), Decision::Swallow);
        assert!(g.fail_open());
        assert_eq!(g.on_key0(key(0x43, false)), Decision::Pass);
        assert_eq!(g.queued(), 0);
        assert!(g.take_failed());
        assert!(g.is_passing());
        assert!(!g.fail_open(), "nothing held: no reason to fail open");
    }

    #[test]
    fn undo_holds_the_hangul_key_up_too() {
        let mut g = Gate::new();
        g.undo_until_ms = 2000;
        assert_eq!(g.on_key(key(VK_HANGUL, false), 100), Decision::Swallow);
        g.next();
        let e = g.commit0(0, 100);
        g.sent(e, 0, 0, 100);
        assert!(g.is_passing());
        assert_eq!(g.on_key(key(VK_HANGUL, true), 150), Decision::Swallow);
    }

    #[test]
    fn watchdog_bumps_epoch_and_ignores_stale_echoes() {
        let mut g = Gate::new();
        g.on_key0(key(0x41, false));
        g.on_key0(key(VK_SPACE, false));
        g.next();
        g.next();
        let old = g.commit0(2, 0);
        assert!(!g.watchdog(1000), "no reset while the send is still under way");
        g.sent(old, 2, 2, 0);
        assert!(!g.watchdog(100));
        assert!(g.watchdog(1000));
        assert!(g.is_passing());
        g.on_key0(key(0x41, false));
        g.on_key0(key(VK_SPACE, false));
        g.next();
        g.next();
        let e = g.commit0(1, 1000);
        g.sent(e, 1, 1, 1000);
        g.on_echo(old, 1001); // late echo from before the reset must not count
        assert_eq!(g.in_flight(), 1);
    }

    // ---- Randomized simulation of the Windows input pipeline -------------------------------

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn chance(&mut self, pct: u64) -> bool {
            self.next() % 100 < pct
        }
    }

    /// What the target application receives.
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Delivered {
        Physical(u32),
        /// Correction text injected in front of the held physical event with this id.
        Correction(u32),
        /// Mid-word correction injected right after the physical event with this id.
        Live(u32),
    }

    #[derive(Debug, Clone, Copy)]
    enum SysEvent {
        Physical(u32, Input),
        Injected { epoch: u32, what: Delivered },
    }

    /// Runs one scenario. `buggy` lets physical input pass while injections are in flight, which
    /// the simulation must catch (proves the test has teeth).
    fn simulate(seed: u64, steps: usize, buggy: bool, lossy: bool) -> Result<(), String> {
        simulate_with(seed, steps, buggy, lossy, false).map(|_| ())
    }

    /// Returns the number of physical events the app never received.
    fn simulate_with(seed: u64, steps: usize, buggy: bool, lossy: bool, stalls: bool) -> Result<usize, String> {
        let mut rng = Rng(seed | 1);
        let mut gate = Gate::new();
        let mut sys: VecDeque<SysEvent> = VecDeque::new(); // raw input thread queue
        let mut app: Vec<Delivered> = Vec::new();
        // Worker: committed events waiting for the SendInput call (the commit->append window).
        let mut pending_send: Option<(u32, Vec<Delivered>)> = None;
        let mut ids: VecDeque<u32> = VecDeque::new(); // physical ids in the gate stream, in order
        let mut next_id = 0u32;
        let mut now = 0u64;
        let mut lost = std::collections::HashSet::new();
        let mut last_seen: Option<u32> = None; // last physical id the worker processed
        let mut unblock_after_send = false;

        for _ in 0..steps {
            now += rng.next() % 3;
            match rng.next() % 6 {
                // A physical event happens.
                0 | 1 => {
                    let input = match rng.next() % 10 {
                        0..=4 => Input::Key(key(0x41 + (rng.next() % 26) as u16, rng.chance(40))),
                        5..=7 => Input::Key(key(VK_SPACE, rng.chance(40))),
                        8 => Input::Key(key(VK_LSHIFT, rng.chance(50))),
                        _ => Input::Mouse(MouseButton { flags: 2, data: 0, x: 1, y: 1 }),
                    };
                    sys.push_back(SysEvent::Physical(next_id, input));
                    next_id += 1;
                }
                // The raw input thread runs our hook on the oldest event.
                2 => match sys.pop_front().inspect(|_| gate.touch(now)) {
                    Some(SysEvent::Physical(id, input)) => {
                        let d = if buggy && gate.held == 0 && gate.in_flight > 0 {
                            gate.stream.push_back(Item::Passed(input));
                            Decision::Pass
                        } else {
                            match input {
                                Input::Key(k) => gate.on_key(k, now),
                                Input::Mouse(m) => gate.on_mouse_button(m, now),
                            }
                        };
                        ids.push_back(id);
                        if d == Decision::Pass {
                            app.push(Delivered::Physical(id));
                        }
                    }
                    Some(SysEvent::Injected { epoch, what }) => {
                        if lossy && rng.chance(3) {
                            // Echo lost (hook skipped); the app still gets it.
                        } else {
                            gate.on_echo(epoch, now);
                        }
                        app.push(what);
                    }
                    None => {}
                },
                // Worker processes the stream.
                3 | 4 => {
                    if let Some((epoch, events)) = pending_send.take() {
                        let n = events.len() as u32;
                        if lossy && rng.chance(2) {
                            // SendInput dropped everything (UIPI-like).
                            gate.sent(epoch, n, 0, now);
                            for e in &events {
                                if let Delivered::Physical(id) = e {
                                    lost.insert(*id);
                                }
                            }
                        } else {
                            for what in events {
                                sys.push_back(SysEvent::Injected { epoch, what });
                            }
                            gate.sent(epoch, n, n, now);
                        }
                        if std::mem::take(&mut unblock_after_send) {
                            gate.set_blocked(false);
                        }
                    } else if !buggy && rng.chance(10) && gate.held == 0 {
                        // Mid-word correction: block, drain what already passed, then inject.
                        let generation = gate.begin_unsolicited();
                        while let Some(Item::Passed(_)) = gate.stream.front() {
                            gate.next();
                            last_seen = ids.pop_front();
                        }
                        if let Some(after) = last_seen {
                            let epoch = gate.reserve(1, now, generation).unwrap();
                            pending_send = Some((epoch, vec![Delivered::Live(after)]));
                            unblock_after_send = true;
                        } else {
                            gate.set_blocked(false);
                        }
                    } else if let Some(item) = gate.next() {
                        let id = ids.pop_front().unwrap();
                        last_seen = Some(id);
                        if let Item::Held(input) = item {
                            if rng.chance(30) {
                                ids.push_front(id); // still deciding; item stays held
                                continue;
                            }
                            last_seen = Some(id);
                            let mut events = Vec::new();
                            if matches!(input, Input::Key(Key { vk: VK_SPACE, up: false, .. })) && rng.chance(50) {
                                events.push(Delivered::Correction(id));
                            }
                            events.push(Delivered::Physical(id));
                            let epoch = gate.commit0(events.len() as u32, now);
                            pending_send = Some((epoch, events));
                        }
                    }
                }
                _ => {
                    // Long pauses only happen when the input queue is idle, unless simulating a stall.
                    if sys.is_empty() || stalls {
                        now += rng.next() % 200;
                    }
                    gate.watchdog(now);
                }
            }
        }

        if stalls {
            return Ok(lost.len());
        }
        if std::env::var("GATE_TRACE").is_ok() {
            eprintln!("{app:?}");
        }
        // Check: physical events reach the app in physical order; a correction comes right before its key.
        let mut last = None;
        for (i, d) in app.iter().enumerate() {
            match *d {
                Delivered::Physical(id) => {
                    if let Some(prev) = last {
                        if id <= prev {
                            return Err(format!("seed {seed}: physical {id} after {prev}"));
                        }
                    }
                    last = Some(id);
                }
                Delivered::Live(after) => {
                    // Everything up to `after` was on screen, nothing after it yet.
                    let after_was_dropped = lost.contains(&after) && last.is_none_or(|l| l < after);
                    if last != Some(after) && !after_was_dropped {
                        return Err(format!("seed {seed}: live fix after {after} landed after {last:?}"));
                    }
                }
                Delivered::Correction(id) => {
                    // (The key may still be in the system queue when the run ends.)
                    if app.get(i + 1).is_some_and(|next| *next != Delivered::Physical(id)) {
                        return Err(format!("seed {seed}: correction for {id} not directly before it"));
                    }
                }
            }
        }
        // Drain: everything held must eventually come out (no deadlock).
        let delivered = app.iter().filter(|d| matches!(d, Delivered::Physical(_))).count();
        let in_system = sys.iter().filter(|e| matches!(e, SysEvent::Physical(..) | SysEvent::Injected { what: Delivered::Physical(_), .. })).count();
        let accounted = delivered + lost.len() + in_system + gate.queued() + pending_send.map_or(0, |p| p.1.len());
        if accounted < next_id as usize {
            return Err(format!("seed {seed}: events vanished ({accounted} of {next_id})"));
        }
        Ok(lost.len())
    }

    #[test]
    #[ignore]
    fn trace_one() {
        let seed: u64 = std::env::var("GATE_SEED").unwrap().parse().unwrap();
        simulate(seed, 400, false, false).unwrap();
    }

    #[test]
    fn simulation_preserves_order() {
        for seed in 1..=3000u64 {
            simulate(seed.wrapping_mul(0x9E3779B97F4A7C15), 400, false, false).unwrap();
        }
    }

    #[test]
    fn simulation_preserves_order_with_lost_echoes_and_drops() {
        for seed in 1..=3000u64 {
            simulate(seed.wrapping_mul(0xD1B54A32D192ED03), 400, false, true).unwrap();
        }
    }

    #[test]
    fn simulation_survives_system_stalls() {
        // Under arbitrary stalls the watchdog may reorder (logged degraded mode) but must never hang.
        for seed in 1..=500u64 {
            simulate_with(seed.wrapping_mul(0x9E3779B97F4A7C15), 400, false, true, true).unwrap();
        }
    }

    #[test]
    fn simulation_catches_passing_while_in_flight() {
        let caught = (1..=500u64).any(|seed| simulate(seed.wrapping_mul(0x9E3779B97F4A7C15), 400, true, false).is_err());
        assert!(caught, "the buggy gate must be detected");
    }
}
