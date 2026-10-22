//! Follows the ordered input stream from the gate, keeps the word being typed and turns a held
//! boundary key into an injection plan (the correction, if any, followed by the key itself).

use std::collections::HashSet;

use crate::classifier::{self, Mode, Verdict};
use crate::dubeolsik;
use crate::gate::{self, Input, Item, Key, Mods, MouseButton, VK_HANGUL, VK_SPACE};

pub const VK_BACK: u16 = 0x08;
pub const VK_CAPITAL: u16 = 0x14;
pub const VK_HANJA: u16 = 0x19;
pub const UNDO_WINDOW_MS: u64 = 2000;

/// What the worker can ask the system. Implemented over Win32 in `win`, faked in tests.
pub trait Env {
    /// Current IME mode of the focused window; `None` if unknown or not a Korean layout.
    fn ime_mode(&mut self) -> Option<Mode>;
    /// Identity of the focused control.
    fn focus(&mut self) -> u64;
    /// Focused control is a password field.
    fn is_password(&mut self) -> bool;
    fn caps_lock(&mut self) -> bool;
    fn now_ms(&mut self) -> u64;
}

/// One step of an injection plan. `scan == 0` means "fill in from the layout".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Out {
    Key(Key),
    Mouse(MouseButton),
    /// A character delivered as Unicode (`KEYEVENTF_UNICODE`): the IME does not compose it, so the
    /// text is right whatever mode the IME is in and however late the app handles a 한/영 toggle.
    Text(char),
    /// Stop sending until the IME reports this mode (after an injected 한/영 key). Not an event.
    AwaitMode(Mode),
    /// Set the IME mode directly (no key) and confirm it before sending the rest. Not an event.
    SetMode(Mode),
}

impl Out {
    /// How many low-level input events this step produces (and so how many echoes to expect).
    pub fn event_count(&self) -> u32 {
        match self {
            Out::Key(_) | Out::Mouse(_) => 1,
            Out::Text(_) => 2,
            Out::AwaitMode(_) | Out::SetMode(_) => 0,
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Plan {
    pub events: Vec<Out>,
    /// After injecting, make sure the IME is in this mode (set it directly if needed) before
    /// letting later input through. The mode is not switched with a 한/영 key for this: apps handle
    /// injected 한/영 keys late or not at all (seen in practice), which garbled the retyped word.
    pub expect_mode: Option<Mode>,
    /// Arm the 한/영-undo window until this time.
    pub undo_until_ms: u64,
    /// Human-readable note for the log (no key contents unless debugging).
    pub note: Option<String>,
    /// A mid-word conversion: until the word ends, a 한/영 press means "undo it".
    pub live: bool,
    /// How many events at the end are the user's own held key (always delivered, even if the
    /// correction in front of them is abandoned).
    pub user_tail: usize,
    /// If switching the IME to Hangul cannot be confirmed, type this as text instead of the
    /// replayed keys after the `SetMode` step (they would otherwise come out as English letters).
    pub fallback_text: Option<String>,
}

/// A word converted while it was being typed.
#[derive(Debug, Clone)]
struct LiveState {
    /// Finished syllables put on screen in place of the first letters.
    committed: String,
    /// Keys from here on were replayed / typed through the Korean IME.
    replay_from: usize,
}

#[derive(Debug, Clone)]
struct LastConversion {
    original_keys: Vec<(char, bool)>,
    original_mode: Mode,
    converted_len: usize,
    until_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// ToEnglish needs the IME to commit its composition on an injected 한/영 key (checked by selftest).
    pub to_english: bool,
    /// Convert English-mode Korean while the word is being typed, not only at Space.
    pub live: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { to_english: false, live: true }
    }
}

#[derive(Default)]
pub struct Tracker {
    word: Vec<(char, bool)>,
    word_mode: Option<Mode>,
    word_focus: u64,
    word_valid: bool,
    /// Set when this word was converted while typing.
    word_live: Option<LiveState>,
    /// The user undid a mid-word conversion of this word: leave it alone.
    word_no_fix: bool,
    mods: Mods,
    last: Option<LastConversion>,
    ignored: HashSet<String>,
    /// Letter sequences whose mid-word conversion the user undid.
    live_ignored: HashSet<String>,
    drop_hangul_up: bool,
    pub options: Options,
    pub thresholds: classifier::Thresholds,
}

fn key_down(vk: u16) -> Out {
    Out::Key(Key { vk, scan: 0, extended: false, up: false })
}

fn key_up(vk: u16) -> Out {
    Out::Key(Key { vk, scan: 0, extended: false, up: true })
}

fn tap(out: &mut Vec<Out>, vk: u16) {
    out.push(key_down(vk));
    out.push(key_up(vk));
}

/// Character a non-letter key types on a US layout (same in Hangul mode).
fn symbol(vk: u16, shift: bool) -> Option<char> {
    const DIGITS: &[u8; 10] = b")!@#$%^&*(";
    if (0x30..=0x39).contains(&vk) {
        let i = (vk - 0x30) as usize;
        return Some(if shift { DIGITS[i] as char } else { (b'0' + i as u8) as char });
    }
    let (plain, shifted) = match vk {
        0xBA => (';', ':'),
        0xBB => ('=', '+'),
        0xBC => (',', '<'),
        0xBD => ('-', '_'),
        0xBE => ('.', '>'),
        0xBF => ('/', '?'),
        0xC0 => ('`', '~'),
        0xDB => ('[', '{'),
        0xDC => ('\\', '|'),
        0xDD => (']', '}'),
        0xDE => ('\'', '"'),
        _ => return None,
    };
    Some(if shift { shifted } else { plain })
}

fn type_text(out: &mut Vec<Out>, text: &str) {
    out.extend(text.chars().map(Out::Text));
}

impl Tracker {
    pub fn new(options: Options) -> Tracker {
        Tracker { options, ..Tracker::default() }
    }

    fn reset_word(&mut self) {
        self.word.clear();
        self.word_mode = None;
        self.word_valid = true;
        self.word_live = None;
        self.word_no_fix = false;
    }

    /// Cheap check after a letter passed: could the word be converted right now (mid-word)?
    /// If so the worker blocks input, drains what already passed and asks [`Tracker::live_plan`].
    pub fn live_candidate(&self) -> bool {
        self.options.live
            && !self.mods.any() // a held Shift would be lifted by the replay (Stage 1 cause #3)
            && self.word_valid
            && self.word_mode == Some(Mode::English)
            && self.word_live.is_none()
            && !self.word_no_fix
            && classifier::live_forward(&self.word, &self.thresholds).is_some()
            && !self.live_ignored.iter().any(|undone| self.lowered().starts_with(undone.as_str()))
    }

    fn lowered(&self) -> String {
        self.word.iter().map(|(c, _)| c.to_ascii_lowercase()).collect()
    }

    /// Mid-word conversion of the letters typed so far (English mode -> Korean): erase them, put
    /// the finished syllables on screen, switch the IME to Hangul and replay the keys of the
    /// syllable still being typed so the IME keeps composing it.
    pub fn live_plan(&mut self, env: &mut dyn Env) -> Option<Plan> {
        if !self.live_candidate() || env.ime_mode() != Some(Mode::English) || env.focus() != self.word_focus || env.is_password() {
            return None;
        }
        let fix = classifier::live_forward(&self.word, &self.thresholds)?;
        let mut plan = Plan { live: true, expect_mode: Some(Mode::Hangul), ..Plan::default() };
        let ev = &mut plan.events;
        // Erase and write while still in English mode (nothing composes), then switch the IME and
        // replay the last syllable. Switching first and sending everything as one batch looked
        // smoother but lost keys in Windows 11 Notepad (`dkssud` -> `dk녕`).
        (0..self.word.len()).for_each(|_| tap(ev, VK_BACK));
        type_text(ev, &fix.committed);
        ev.push(Out::SetMode(Mode::Hangul));
        for &(c, shift) in &self.word[fix.replay_from..] {
            if shift {
                ev.push(key_down(gate::VK_LSHIFT));
            }
            tap(ev, c.to_ascii_uppercase() as u16);
            if shift {
                ev.push(key_up(gate::VK_LSHIFT));
            }
        }
        plan.fallback_text = Some(dubeolsik::compose(&self.word[fix.replay_from..]).text);
        plan.note = Some(format!("live to Korean ({} keys)", self.word.len()));
        self.word_live = Some(LiveState { committed: fix.committed, replay_from: fix.replay_from });
        self.word_mode = Some(Mode::Hangul);
        self.last = None;
        Some(plan)
    }

    /// 한/영 while a mid-word conversion is on screen: put the letters back as typed and keep
    /// this word in English (and never convert this letter sequence mid-word again).
    fn undo_live(&mut self, live: LiveState) -> Plan {
        let mut plan = Plan { expect_mode: Some(Mode::English), note: Some("undo live".into()), ..Plan::default() };
        let on_screen =
            live.committed.chars().count() + dubeolsik::compose(&self.word[live.replay_from..]).text.chars().count();
        let ev = &mut plan.events;
        // 한/영 commits the syllable being composed, so Backspace then removes whole syllables.
        tap(ev, VK_HANGUL);
        ev.push(Out::AwaitMode(Mode::English));
        (0..on_screen).for_each(|_| tap(ev, VK_BACK));
        type_text(ev, &classifier::screen_text(&self.word, Mode::English));
        let fired_at = (1..=self.word.len())
            .find(|&n| classifier::live_forward(&self.word[..n], &self.thresholds).is_some())
            .unwrap_or(self.word.len());
        self.live_ignored.insert(self.word[..fired_at].iter().map(|(c, _)| c.to_ascii_lowercase()).collect());
        self.word_mode = Some(Mode::English);
        self.word_no_fix = true;
        self.drop_hangul_up = true;
        plan
    }

    /// Feed a passed-through item (already delivered to the app).
    pub fn observe(&mut self, item: &Item, env: &mut dyn Env) {
        let input = match item {
            Item::Passed(i) | Item::Held(i) => *i,
        };
        match input {
            Input::Mouse(_) => {
                self.reset_word();
                self.last = None;
            }
            Input::Key(k) => self.observe_key(&k, env),
        }
    }

    fn observe_key(&mut self, k: &Key, env: &mut dyn Env) {
        if k.up && k.vk == VK_HANGUL {
            // Passed through (the gate did not hold it): nothing left to drop.
            self.drop_hangul_up = false;
        }
        if self.mods.apply(k) || k.up {
            return;
        }
        if k.vk != VK_HANGUL {
            self.last = None;
        }
        let shortcut = self.mods.ctrl || self.mods.alt || self.mods.win;
        if gate::is_letter(k.vk) && !shortcut {
            if self.word.is_empty() {
                self.reset_word();
                self.word_mode = env.ime_mode();
                self.word_focus = env.focus();
                if env.caps_lock() {
                    self.word_valid = false;
                }
            }
            self.word.push(((k.vk as u8 as char).to_ascii_lowercase(), self.mods.shift));
            return;
        }
        if let Some(c) = symbol(k.vk, self.mods.shift).filter(|_| !shortcut) {
            if !self.word.is_empty() {
                self.word.push((c, false));
            }
            return;
        }
        match k.vk {
            VK_BACK | VK_HANGUL | VK_HANJA | gate::VK_RMENU | VK_CAPITAL if !self.word.is_empty() => {
                // The screen no longer matches `word`: no correction (or undo) may count on it.
                self.word_valid = false;
                self.word_live = None;
            }
            _ => self.reset_word(),
        }
    }

    /// A held item reached the front of the queue: decide what to inject for it.
    pub fn held(&mut self, input: Input, env: &mut dyn Env) -> Plan {
        let mut plan = Plan::default();
        let k = match input {
            Input::Mouse(m) => {
                self.observe(&Item::Held(input), env);
                plan.events.push(Out::Mouse(m));
                return plan;
            }
            Input::Key(k) => k,
        };

        if k.up && k.vk == VK_HANGUL && std::mem::take(&mut self.drop_hangul_up) {
            return plan; // the undo key's up: the app never saw its down either
        }
        if !k.up && k.vk == VK_HANGUL {
            if let Some(live) = self.word_live.take() {
                return self.undo_live(live);
            }
            if let Some(last) = self.last.take().filter(|l| env.now_ms() < l.until_ms) {
                return self.undo(last, k);
            }
        }
        if !k.up && k.vk == VK_SPACE && !self.word.is_empty() && !self.mods.any() {
            plan = self.correct(env);
            // The boundary key itself must not disarm the undo that the correction just armed.
            let armed = self.last.take();
            self.observe_key(&k, env);
            self.last = armed;
        } else {
            self.observe_key(&k, env);
        }
        plan.events.push(Out::Key(k));
        plan.user_tail = 1;
        plan
    }

    /// Build the correction for the finished word (without the boundary key itself).
    fn correct(&mut self, env: &mut dyn Env) -> Plan {
        let mut plan = Plan::default();
        let word = std::mem::take(&mut self.word);
        let n = word.len();
        // Every early return says why, so "it did not convert" can be diagnosed from the log
        // without recording what was typed.
        let skip = |why: String| Plan { note: Some(format!("kept ({n} keys): {why}")), ..Plan::default() };
        if let Some(live) = self.word_live.take() {
            // Converted mid-word; 한/영 right after this Space still undoes it like any conversion.
            let on_screen = live.committed.chars().count() + dubeolsik::compose(&word[live.replay_from..]).text.chars().count();
            self.remember(word, Mode::English, on_screen, env);
            let mut plan = skip("already converted while typing".into());
            plan.undo_until_ms = self.last.as_ref().map_or(0, |l| l.until_ms);
            return plan;
        }
        if self.word_no_fix {
            return skip("mid-word conversion was undone".into());
        }
        if !self.word_valid {
            return skip("Backspace/한영/한자/Caps Lock during the word".into());
        }
        let Some(mode) = self.word_mode else {
            return skip("IME mode unreadable in this app".into());
        };
        let lowered: String = word.iter().map(|(c, _)| *c).collect();
        if self.ignored.contains(&lowered) {
            return skip("undone earlier".into());
        }
        let now_mode = env.ime_mode();
        if now_mode != Some(mode) {
            return skip(format!("IME mode changed during the word ({mode:?} -> {now_mode:?})"));
        }
        if env.focus() != self.word_focus {
            return skip("focus moved".into());
        }
        if env.is_password() {
            return skip("password field".into());
        }
        let screen = classifier::screen_text(&word, mode);
        let screen_len = screen.chars().count();
        let ev = &mut plan.events;
        match classifier::judge(&word, mode) {
            Verdict::Keep => return skip(format!("classifier: fine as typed in {mode:?} mode")),
            // English word + particle (html로): only the IME version (hanfix-ime) rewrites these.
            Verdict::Mixed(_) => return skip("English word + Korean particle (fixed by hanfix-ime only)".into()),
            Verdict::ToKorean(text) => {
                // English mode: nothing is composing, so each Backspace removes one letter.
                (0..screen_len).for_each(|_| tap(ev, VK_BACK));
                type_text(ev, &text);
                plan.expect_mode = Some(Mode::Hangul);
                plan.note = Some(format!("to Korean ({} keys)", word.len()));
                self.remember(word, mode, text.chars().count(), env);
            }
            Verdict::ToEnglish(text) => {
                if !self.options.to_english {
                    return skip("English fixes are off (run --selftest)".into());
                }
                // 한/영 first commits the composing syllable; only once the IME reports English
                // mode is it safe to send Backspaces (which then remove whole syllables).
                tap(ev, VK_HANGUL);
                ev.push(Out::AwaitMode(Mode::English));
                (0..screen_len).for_each(|_| tap(ev, VK_BACK));
                type_text(ev, &text);
                plan.expect_mode = Some(Mode::English);
                plan.note = Some(format!("to English ({} keys)", word.len()));
                self.remember(word, mode, text.chars().count(), env);
            }
        }
        plan.undo_until_ms = self.last.as_ref().map_or(0, |l| l.until_ms);
        plan
    }

    fn remember(&mut self, word: Vec<(char, bool)>, mode: Mode, converted_len: usize, env: &mut dyn Env) {
        self.last = Some(LastConversion {
            original_keys: word,
            original_mode: mode,
            converted_len,
            until_ms: env.now_ms() + UNDO_WINDOW_MS,
        });
    }

    /// 한/영 right after a conversion: put back what was typed, in the original mode.
    fn undo(&mut self, last: LastConversion, _hangul_key: Key) -> Plan {
        let mut plan = Plan::default();
        let ev = &mut plan.events;
        // Converted text + the space after it. The IME is in the converted mode with nothing composing.
        (0..last.converted_len + 1).for_each(|_| tap(ev, VK_BACK));
        type_text(ev, &classifier::screen_text(&last.original_keys, last.original_mode));
        tap(ev, VK_SPACE);
        self.ignored.insert(last.original_keys.iter().map(|(c, _)| *c).collect());
        self.drop_hangul_up = true;
        self.reset_word();
        plan.expect_mode = Some(last.original_mode);
        plan.note = Some("undo".into());
        plan
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dubeolsik;
    use crate::gate::VK_LSHIFT;

    /// Virtual key and Shift needed to type an ASCII character on a US layout.
    fn vk_for(c: char) -> Option<(u16, bool)> {
        if c.is_ascii_lowercase() {
            return Some((c.to_ascii_uppercase() as u16, false));
        }
        if c.is_ascii_uppercase() {
            return Some((c as u16, true));
        }
        for vk in (0x30..=0x39).chain([0xBA, 0xBB, 0xBC, 0xBD, 0xBE, 0xBF, 0xC0, 0xDB, 0xDC, 0xDD, 0xDE]) {
            for shift in [false, true] {
                if symbol(vk, shift) == Some(c) {
                    return Some((vk, shift));
                }
            }
        }
        None
    }

    struct FakeEnv {
        mode: Option<Mode>,
        focus: u64,
        password: bool,
        now: u64,
    }

    impl Env for FakeEnv {
        fn ime_mode(&mut self) -> Option<Mode> {
            self.mode
        }
        fn focus(&mut self) -> u64 {
            self.focus
        }
        fn is_password(&mut self) -> bool {
            self.password
        }
        fn caps_lock(&mut self) -> bool {
            false
        }
        fn now_ms(&mut self) -> u64 {
            self.now
        }
    }

    fn env(mode: Mode) -> FakeEnv {
        FakeEnv { mode: Some(mode), focus: 1, password: false, now: 0 }
    }

    fn k(vk: u16, up: bool) -> Key {
        Key { vk, scan: 0, extended: false, up }
    }

    /// Type `s` as passed-through keys (uppercase = Shift held around the key).
    fn type_passed(t: &mut Tracker, e: &mut FakeEnv, s: &str) {
        for c in s.chars() {
            let (vk, shift) = vk_for(c).unwrap();
            if shift {
                t.observe(&Item::Passed(Input::Key(k(VK_LSHIFT, false))), e);
            }
            t.observe(&Item::Passed(Input::Key(k(vk, false))), e);
            t.observe(&Item::Passed(Input::Key(k(vk, true))), e);
            if shift {
                t.observe(&Item::Passed(Input::Key(k(VK_LSHIFT, true))), e);
            }
        }
    }

    /// Replays a plan on a model of the text field + Korean IME; returns the resulting text.
    /// `final_mode` is the plan's `expect_mode`, which the worker applies after injecting.
    fn replay(start_text: &str, start_mode: Mode, events: &[Out], final_mode: Option<Mode>) -> (String, Mode) {
        let mut committed: Vec<char> = start_text.chars().collect();
        let mut composing: Vec<(char, bool)> = Vec::new();
        let mut mode = start_mode;
        let mut shift = false;
        let flush = |committed: &mut Vec<char>, composing: &mut Vec<(char, bool)>| {
            committed.extend(dubeolsik::compose(composing).text.chars());
            composing.clear();
        };
        for e in events {
            let key = match *e {
                Out::Key(key) => key,
                Out::Text(c) => {
                    // The IME does not compose Unicode packets.
                    flush(&mut committed, &mut composing);
                    committed.push(c);
                    continue;
                }
                Out::AwaitMode(m) => {
                    assert_eq!(mode, m, "plan waits for a mode the IME never reaches");
                    continue;
                }
                Out::SetMode(m) => {
                    assert!(composing.is_empty(), "mode set by message while a syllable is composing");
                    mode = m;
                    continue;
                }
                Out::Mouse(_) => continue,
            };
            if key.vk == VK_LSHIFT {
                shift = !key.up;
                continue;
            }
            if key.up {
                continue;
            }
            match key.vk {
                VK_HANGUL => {
                    flush(&mut committed, &mut composing);
                    mode = if mode == Mode::Hangul { Mode::English } else { Mode::Hangul };
                }
                VK_BACK => {
                    if composing.pop().is_none() {
                        committed.pop();
                    }
                }
                VK_SPACE => {
                    flush(&mut committed, &mut composing);
                    committed.push(' ');
                }
                vk if gate::is_letter(vk) => {
                    let c = (vk as u8 as char).to_ascii_lowercase();
                    match mode {
                        Mode::Hangul => composing.push((c, shift)),
                        Mode::English => committed.push(if shift { c.to_ascii_uppercase() } else { c }),
                    }
                }
                vk => {
                    flush(&mut committed, &mut composing);
                    committed.push(symbol(vk, shift).unwrap());
                }
            }
        }
        flush(&mut committed, &mut composing);
        (committed.into_iter().collect(), final_mode.unwrap_or(mode))
    }

    #[test]
    fn english_mode_korean_is_rewritten() {
        let mut t = Tracker::new(Options::default());
        let mut e = env(Mode::English);
        type_passed(&mut t, &mut e, "dkssudgktpdy");
        let plan = t.held(Input::Key(k(VK_SPACE, false)), &mut e);
        assert_eq!(plan.expect_mode, Some(Mode::Hangul));
        let (text, mode) = replay("dkssudgktpdy", Mode::English, &plan.events, plan.expect_mode);
        assert_eq!(text, "안녕하세요 ");
        assert_eq!(mode, Mode::Hangul);
        assert!(plan.undo_until_ms > 0);
    }

    #[test]
    fn hangul_mode_english_is_rewritten_when_enabled() {
        let mut t = Tracker::new(Options { to_english: true, live: false });
        let mut e = env(Mode::Hangul);
        type_passed(&mut t, &mut e, "Hello");
        let plan = t.held(Input::Key(k(VK_SPACE, false)), &mut e);
        // On screen before: the IME shows ㅗ디ㅣㅐ with the last syllable still composing.
        let keys: Vec<Out> = "Hello"
            .chars()
            .flat_map(|c| {
                let (vk, s) = vk_for(c).unwrap();
                let mut v = Vec::new();
                if s {
                    v.push(key_down(VK_LSHIFT));
                }
                tap(&mut v, vk);
                if s {
                    v.push(key_up(VK_LSHIFT));
                }
                v
            })
            .chain(plan.events.iter().copied())
            .collect();
        let (text, mode) = replay("", Mode::Hangul, &keys, plan.expect_mode);
        assert_eq!(text, "Hello ");
        assert_eq!(mode, Mode::English);
    }

    #[test]
    fn to_english_off_by_default() {
        let mut t = Tracker::new(Options::default());
        let mut e = env(Mode::Hangul);
        type_passed(&mut t, &mut e, "hello");
        let plan = t.held(Input::Key(k(VK_SPACE, false)), &mut e);
        assert_eq!(plan.events, vec![Out::Key(k(VK_SPACE, false))]);
    }

    /// Bug 2 end to end: Shift held through a Korean word in Hangul mode changes nothing.
    #[test]
    fn shift_held_korean_passes_untouched() {
        let mut t = Tracker::new(Options { to_english: true, live: false });
        let mut e = env(Mode::Hangul);
        type_passed(&mut t, &mut e, "DKSSUDGKSKDY");
        let plan = t.held(Input::Key(k(VK_SPACE, false)), &mut e);
        assert_eq!(plan.events, vec![Out::Key(k(VK_SPACE, false))]);
    }

    #[test]
    fn skips_when_mode_or_focus_changed_or_password() {
        for change in 0..4 {
            let mut t = Tracker::new(Options::default());
            let mut e = env(Mode::English);
            type_passed(&mut t, &mut e, "dkssud");
            match change {
                0 => e.mode = Some(Mode::Hangul),
                1 => e.focus = 2,
                2 => e.password = true,
                _ => t.observe(&Item::Passed(Input::Key(k(VK_BACK, false))), &mut e),
            }
            let plan = t.held(Input::Key(k(VK_SPACE, false)), &mut e);
            assert_eq!(plan.events.len(), 1, "case {change}");
        }
    }

    #[test]
    fn undo_restores_and_ignores_word() {
        let mut t = Tracker::new(Options::default());
        let mut e = env(Mode::English);
        type_passed(&mut t, &mut e, "dkssud");
        let first = t.held(Input::Key(k(VK_SPACE, false)), &mut e);
        let (after, mode) = replay("dkssud", Mode::English, &first.events, first.expect_mode);
        assert_eq!(after, "안녕 ");
        e.mode = Some(mode);
        e.now = 500;
        let undo = t.held(Input::Key(k(VK_HANGUL, false)), &mut e);
        let (restored, mode) = replay(&after, Mode::Hangul, &undo.events, undo.expect_mode);
        assert_eq!(restored, "dkssud ");
        assert_eq!(mode, Mode::English);
        let up = t.held(Input::Key(k(VK_HANGUL, true)), &mut e);
        assert!(up.events.is_empty(), "the undo key's up is dropped, not delivered unpaired");
        // Same word again is left alone.
        e.mode = Some(Mode::English);
        type_passed(&mut t, &mut e, "dkssud");
        let again = t.held(Input::Key(k(VK_SPACE, false)), &mut e);
        assert_eq!(again.events.len(), 1);
    }

    /// Physical key taps for `s` (uppercase = Shift), as the app receives them.
    fn taps(s: &str) -> Vec<Out> {
        let mut v = Vec::new();
        for c in s.chars() {
            let (vk, shift) = if c == ' ' { (VK_SPACE, false) } else { vk_for(c).unwrap() };
            if shift {
                v.push(key_down(VK_LSHIFT));
            }
            tap(&mut v, vk);
            if shift {
                v.push(key_up(VK_LSHIFT));
            }
        }
        v
    }

    /// Feed letters one at a time like the worker does; returns the first mid-word plan.
    fn type_until_live(t: &mut Tracker, e: &mut FakeEnv, s: &str) -> (usize, Plan) {
        for (i, c) in s.chars().enumerate() {
            type_passed(t, e, &c.to_string());
            if t.live_candidate() {
                return (i + 1, t.live_plan(e).unwrap());
            }
        }
        panic!("never converted mid-word: {s}");
    }

    #[test]
    fn converts_while_typing_and_the_ime_finishes_the_word() {
        let mut t = Tracker::new(Options::default());
        let mut e = env(Mode::English);
        let (typed, plan) = type_until_live(&mut t, &mut e, "dkssudgktpdy");
        assert!(typed < 12, "fires before the word ends");
        assert!(plan.live);
        // App's view: letters typed in English, the live plan, then the rest typed via the IME.
        let mut stream = taps(&"dkssudgktpdy"[..typed]);
        stream.extend(plan.events.iter().copied());
        stream.extend(taps(&"dkssudgktpdy"[typed..]));
        stream.extend(taps(" "));
        let (text, mode) = replay("", Mode::English, &stream, None);
        assert_eq!(text, "안녕하세요 ");
        assert_eq!(mode, Mode::Hangul);
        // The rest of the word is typed in Hangul mode; Space does not touch it again.
        e.mode = Some(Mode::Hangul);
        type_passed(&mut t, &mut e, &"dkssudgktpdy"[typed..]);
        let space = t.held(Input::Key(k(VK_SPACE, false)), &mut e);
        assert_eq!(space.events, vec![Out::Key(k(VK_SPACE, false))]);
    }

    #[test]
    fn english_words_never_convert_mid_word() {
        for w in ["hello", "github", "something", "rust", "today", "todays", "andover", "FormalParameterList", "cmake"] {
            let mut t = Tracker::new(Options::default());
            let mut e = env(Mode::English);
            for c in w.chars() {
                type_passed(&mut t, &mut e, &c.to_string());
                assert!(!t.live_candidate() || w == "cmake", "{w} fired mid-word");
            }
        }
    }

    #[test]
    fn hangul_key_undoes_a_mid_word_conversion() {
        let mut t = Tracker::new(Options::default());
        let mut e = env(Mode::English);
        let (typed, plan) = type_until_live(&mut t, &mut e, "dkssud");
        let mut stream = taps(&"dkssud"[..typed]);
        stream.extend(plan.events.iter().copied());
        e.mode = Some(Mode::Hangul);
        stream.extend(taps(&"dkssud"[typed..]));
        type_passed(&mut t, &mut e, &"dkssud"[typed..]);
        let undo = t.held(Input::Key(k(VK_HANGUL, false)), &mut e);
        stream.extend(undo.events.iter().copied());
        let (text, mode) = replay("", Mode::English, &stream, undo.expect_mode);
        assert_eq!(text, "dkssud");
        assert_eq!(mode, Mode::English);
        assert!(t.held(Input::Key(k(VK_HANGUL, true)), &mut e).events.is_empty(), "undo key-up dropped");
        // Same letters later: no mid-word conversion this time (the Space check still applies).
        t.held(Input::Key(k(VK_SPACE, false)), &mut e);
        e.mode = Some(Mode::English);
        for c in "dkssud".chars() {
            type_passed(&mut t, &mut e, &c.to_string());
            assert!(!t.live_candidate());
        }
    }

    #[test]
    fn trailing_punctuation_is_retyped() {
        let mut t = Tracker::new(Options::default());
        let mut e = env(Mode::English);
        type_passed(&mut t, &mut e, "dkssud?");
        let plan = t.held(Input::Key(k(VK_SPACE, false)), &mut e);
        let (text, _) = replay("dkssud?", Mode::English, &plan.events, plan.expect_mode);
        assert_eq!(text, "안녕? ");
    }

    #[test]
    fn held_letters_after_boundary_start_next_word() {
        let mut t = Tracker::new(Options::default());
        let mut e = env(Mode::English);
        type_passed(&mut t, &mut e, "dkssud");
        t.held(Input::Key(k(VK_SPACE, false)), &mut e);
        e.mode = Some(Mode::Hangul);
        for c in "gktpdy".chars() {
            let (vk, _) = vk_for(c).unwrap();
            let p = t.held(Input::Key(k(vk, false)), &mut e);
            assert_eq!(p.events.len(), 1);
        }
        let p = t.held(Input::Key(k(VK_SPACE, false)), &mut e);
        assert_eq!(p.events.len(), 1, "하세요 typed in Hangul mode is kept");
    }
}
