//! No-panic gate for everything hanfix-ime calls inside other apps' processes (UNIM is built with
//! panic = "abort": a panic there would kill the host app). Random key sequences, including
//! Shift, non-letters and long runs, through every entry point the IME adapter uses.

use hanfix::classifier::{judge, live_forward, Mode, Thresholds};
use hanfix::dubeolsik::{compose, to_keys};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

const KEYS: &[u8] = b"qwertyuiopasdfghjklzxcvbnmQWERTYUIOPASDFGHJKLZXCVBNM0123456789.,?!;:'\"()-_ ";

#[test]
fn random_key_sequences_never_panic() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let t = Thresholds::default();
    for _ in 0..300_000 {
        let len = (rng.next() % 24) as usize;
        let keys: Vec<(char, bool)> = (0..len)
            .map(|_| {
                let c = KEYS[(rng.next() % KEYS.len() as u64) as usize] as char;
                (c.to_ascii_lowercase(), c.is_ascii_uppercase())
            })
            .collect();
        let _ = judge(&keys, Mode::English);
        let _ = judge(&keys, Mode::Hangul);
        for n in 0..=keys.len() {
            let _ = live_forward(&keys[..n], &t);
        }
        let text = compose(&keys).text;
        let _ = to_keys(&text);
    }
}

#[test]
fn odd_inputs_never_panic() {
    let t = Thresholds::default();
    for s in ["", "a", "Q", "qt", "QQQQQQQQ", "ㅄ", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "?", " ", "🙂"] {
        let keys: Vec<(char, bool)> = s.chars().map(|c| (c.to_ascii_lowercase(), c.is_ascii_uppercase())).collect();
        let _ = judge(&keys, Mode::English);
        let _ = judge(&keys, Mode::Hangul);
        let _ = live_forward(&keys, &t);
        let _ = compose(&keys);
        let _ = to_keys(s);
    }
}

#[test]
fn random_letter_words_never_panic() {
    // Letters only, so every check runs its full path (no early exit on punctuation).
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    let t = Thresholds::default();
    let letters = b"qwertyuiopasdfghjklzxcvbnmQWERTOP";
    for _ in 0..200_000 {
        let len = 1 + (rng.next() % 14) as usize;
        let keys: Vec<(char, bool)> = (0..len)
            .map(|_| {
                let c = letters[(rng.next() % letters.len() as u64) as usize] as char;
                (c.to_ascii_lowercase(), c.is_ascii_uppercase())
            })
            .collect();
        let _ = judge(&keys, Mode::English);
        let _ = judge(&keys, Mode::Hangul);
        for n in 0..=keys.len() {
            let _ = live_forward(&keys[..n], &t);
        }
        let _ = to_keys(&compose(&keys).text);
    }
}
