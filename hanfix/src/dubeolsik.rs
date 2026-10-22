//! Dubeolsik (2-set) Korean layout: keys -> jamo -> composed syllables.
//!
//! Mirrors what the Microsoft Korean IME shows on screen for a key sequence,
//! so the classifier can evaluate exactly the text the user saw.

const CHO: [char; 19] = [
    'ㄱ', 'ㄲ', 'ㄴ', 'ㄷ', 'ㄸ', 'ㄹ', 'ㅁ', 'ㅂ', 'ㅃ', 'ㅅ', 'ㅆ', 'ㅇ', 'ㅈ', 'ㅉ', 'ㅊ', 'ㅋ', 'ㅌ', 'ㅍ', 'ㅎ',
];
const JUNG: [char; 21] = [
    'ㅏ', 'ㅐ', 'ㅑ', 'ㅒ', 'ㅓ', 'ㅔ', 'ㅕ', 'ㅖ', 'ㅗ', 'ㅘ', 'ㅙ', 'ㅚ', 'ㅛ', 'ㅜ', 'ㅝ', 'ㅞ', 'ㅟ', 'ㅠ', 'ㅡ', 'ㅢ',
    'ㅣ',
];
const JONG: [char; 28] = [
    '\0', 'ㄱ', 'ㄲ', 'ㄳ', 'ㄴ', 'ㄵ', 'ㄶ', 'ㄷ', 'ㄹ', 'ㄺ', 'ㄻ', 'ㄼ', 'ㄽ', 'ㄾ', 'ㄿ', 'ㅀ', 'ㅁ', 'ㅂ', 'ㅄ', 'ㅅ',
    'ㅆ', 'ㅇ', 'ㅈ', 'ㅊ', 'ㅋ', 'ㅌ', 'ㅍ', 'ㅎ',
];

const VOWEL_PAIRS: [(char, char, char); 7] = [
    ('ㅗ', 'ㅏ', 'ㅘ'),
    ('ㅗ', 'ㅐ', 'ㅙ'),
    ('ㅗ', 'ㅣ', 'ㅚ'),
    ('ㅜ', 'ㅓ', 'ㅝ'),
    ('ㅜ', 'ㅔ', 'ㅞ'),
    ('ㅜ', 'ㅣ', 'ㅟ'),
    ('ㅡ', 'ㅣ', 'ㅢ'),
];
const FINAL_PAIRS: [(char, char, char); 11] = [
    ('ㄱ', 'ㅅ', 'ㄳ'),
    ('ㄴ', 'ㅈ', 'ㄵ'),
    ('ㄴ', 'ㅎ', 'ㄶ'),
    ('ㄹ', 'ㄱ', 'ㄺ'),
    ('ㄹ', 'ㅁ', 'ㄻ'),
    ('ㄹ', 'ㅂ', 'ㄼ'),
    ('ㄹ', 'ㅅ', 'ㄽ'),
    ('ㄹ', 'ㅌ', 'ㄾ'),
    ('ㄹ', 'ㅍ', 'ㄿ'),
    ('ㄹ', 'ㅎ', 'ㅀ'),
    ('ㅂ', 'ㅅ', 'ㅄ'),
];

/// Keys whose Shift variant is a different jamo. Shift on every other key is meaningless in Hangul mode.
pub const SHIFTABLE: [char; 7] = ['q', 'w', 'e', 'r', 't', 'o', 'p'];

/// Jamo produced by a letter key, honoring Shift only where Dubeolsik gives it a meaning.
pub fn jamo(key: char, shift: bool) -> Option<char> {
    let key = key.to_ascii_lowercase();
    if shift {
        let shifted = match key {
            'q' => Some('ㅃ'),
            'w' => Some('ㅉ'),
            'e' => Some('ㄸ'),
            'r' => Some('ㄲ'),
            't' => Some('ㅆ'),
            'o' => Some('ㅒ'),
            'p' => Some('ㅖ'),
            _ => None,
        };
        if shifted.is_some() {
            return shifted;
        }
    }
    Some(match key {
        'q' => 'ㅂ',
        'w' => 'ㅈ',
        'e' => 'ㄷ',
        'r' => 'ㄱ',
        't' => 'ㅅ',
        'y' => 'ㅛ',
        'u' => 'ㅕ',
        'i' => 'ㅑ',
        'o' => 'ㅐ',
        'p' => 'ㅔ',
        'a' => 'ㅁ',
        's' => 'ㄴ',
        'd' => 'ㅇ',
        'f' => 'ㄹ',
        'g' => 'ㅎ',
        'h' => 'ㅗ',
        'j' => 'ㅓ',
        'k' => 'ㅏ',
        'l' => 'ㅣ',
        'z' => 'ㅋ',
        'x' => 'ㅌ',
        'c' => 'ㅊ',
        'v' => 'ㅍ',
        'b' => 'ㅠ',
        'n' => 'ㅜ',
        'm' => 'ㅡ',
        _ => return None,
    })
}

fn is_vowel(j: char) -> bool {
    JUNG.contains(&j)
}

fn can_be_final(c: char) -> bool {
    JONG.contains(&c)
}

fn pair(table: &[(char, char, char)], a: char, b: char) -> Option<char> {
    table.iter().find(|(x, y, _)| *x == a && *y == b).map(|t| t.2)
}

fn split_final(j: char) -> Option<(char, char)> {
    FINAL_PAIRS.iter().find(|t| t.2 == j).map(|t| (t.0, t.1))
}

/// Result of composing a key sequence.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Composed {
    pub text: String,
    /// Number of stray jamo (a consonant or vowel not part of a full syllable).
    pub stray: usize,
    /// Number of complete syllables.
    pub syllables: usize,
}

#[derive(Default)]
struct Builder {
    cho: Option<char>,
    jung: Option<char>,
    jong: Option<char>,
    out: Composed,
}

impl Builder {
    fn flush(&mut self) {
        match (self.cho, self.jung) {
            (Some(c), Some(v)) => {
                let ci = CHO.iter().position(|&x| x == c).unwrap() as u32;
                let vi = JUNG.iter().position(|&x| x == v).unwrap() as u32;
                let ji = self.jong.map_or(0, |j| JONG.iter().position(|&x| x == j).unwrap() as u32);
                self.out.text.push(char::from_u32(0xAC00 + (ci * 21 + vi) * 28 + ji).unwrap());
                self.out.syllables += 1;
            }
            (Some(c), None) => {
                self.out.text.push(c);
                self.out.stray += 1;
            }
            (None, Some(v)) => {
                self.out.text.push(v);
                self.out.stray += 1;
            }
            (None, None) => {
                // A consonant cluster typed without a vowel (`qt` -> ㅄ) shows as one jamo.
                if let Some(j) = self.jong {
                    self.out.text.push(j);
                    self.out.stray += 1;
                }
            }
        }
        self.cho = None;
        self.jung = None;
        self.jong = None;
    }

    fn consonant(&mut self, c: char) {
        match (self.cho, self.jung, self.jong) {
            (Some(_), Some(_), None) if can_be_final(c) => self.jong = Some(c),
            (Some(first), None, None) if pair(&FINAL_PAIRS, first, c).is_some() => {
                self.cho = None;
                self.jong = pair(&FINAL_PAIRS, first, c);
            }
            (Some(_), Some(_), Some(j)) => match pair(&FINAL_PAIRS, j, c) {
                Some(p) => self.jong = Some(p),
                None => {
                    self.flush();
                    self.cho = Some(c);
                }
            },
            _ => {
                self.flush();
                self.cho = Some(c);
            }
        }
    }

    fn vowel(&mut self, v: char) {
        match (self.cho, self.jung, self.jong) {
            (_, None, None) => self.jung = Some(v),
            (_, Some(u), None) => match pair(&VOWEL_PAIRS, u, v) {
                Some(p) => self.jung = Some(p),
                None => {
                    self.flush();
                    self.jung = Some(v);
                }
            },
            (_, Some(_), Some(j)) => {
                // The final consonant moves to start the next syllable.
                let next = match split_final(j) {
                    Some((keep, moved)) => {
                        self.jong = Some(keep);
                        moved
                    }
                    None => {
                        self.jong = None;
                        j
                    }
                };
                self.flush();
                self.cho = Some(next);
                self.jung = Some(v);
            }
            (None, None, Some(j)) => {
                // Lone cluster then a vowel: ㄳ + ㅏ -> ㄱ 사
                let (keep, moved) = split_final(j).expect("lone final is always a cluster");
                self.jong = None;
                self.out.text.push(keep);
                self.out.stray += 1;
                self.cho = Some(moved);
                self.jung = Some(v);
            }
            (Some(_), None, Some(_)) => unreachable!("initial and final without a vowel"),
        }
    }
}

/// Compose a sequence of `(key, shift)` presses as the Korean IME would show it.
/// Non-letter keys are copied through unchanged and end the current syllable.
pub fn compose(keys: &[(char, bool)]) -> Composed {
    let mut b = Builder::default();
    for &(k, shift) in keys {
        match jamo(k, shift) {
            Some(j) if is_vowel(j) => b.vowel(j),
            Some(j) => b.consonant(j),
            None => {
                b.flush();
                b.out.text.push(k);
            }
        }
    }
    b.flush();
    b.out
}

/// Convenience: compose from a plain string where uppercase means Shift.
pub fn compose_str(s: &str) -> Composed {
    let keys: Vec<(char, bool)> = s.chars().map(|c| (c, c.is_ascii_uppercase())).collect();
    compose(&keys)
}

fn key_for(j: char) -> Option<char> {
    "qwertyuiopasdfghjklzxcvbnm"
        .chars()
        .find(|&k| jamo(k, false) == Some(j))
        .or_else(|| SHIFTABLE.iter().copied().find(|&k| jamo(k, true) == Some(j)).map(|k| k.to_ascii_uppercase()))
}

fn push_jamo_keys(out: &mut String, j: char) -> Option<()> {
    if let Some(k) = key_for(j) {
        out.push(k);
        return Some(());
    }
    let (a, b) = VOWEL_PAIRS
        .iter()
        .chain(FINAL_PAIRS.iter())
        .find(|t| t.2 == j)
        .map(|t| (t.0, t.1))?;
    out.push(key_for(a)?);
    out.push(key_for(b)?);
    Some(())
}

/// Keys to type `text` in Dubeolsik (uppercase = Shift). Inverse of [`compose_str`] for Hangul text.
pub fn to_keys(text: &str) -> Option<String> {
    let mut out = String::new();
    for ch in text.chars() {
        let u = ch as u32;
        if (0xAC00..=0xD7A3).contains(&u) {
            let i = u - 0xAC00;
            push_jamo_keys(&mut out, CHO[(i / 588) as usize])?;
            push_jamo_keys(&mut out, JUNG[((i % 588) / 28) as usize])?;
            let jong = JONG[(i % 28) as usize];
            if jong != ' ' {
                push_jamo_keys(&mut out, jong)?;
            }
        } else if CHO.contains(&ch) || JUNG.contains(&ch) || JONG.contains(&ch) {
            push_jamo_keys(&mut out, ch)?;
        } else {
            out.push(ch);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(s: &str) -> String {
        compose_str(s).text
    }

    #[test]
    fn basic_words() {
        assert_eq!(c("dkssudgktpdy"), "안녕하세요");
        assert_eq!(c("gksrmf"), "한글");
        assert_eq!(c("rkqt"), "값");
        assert_eq!(c("rkqtdl"), "값이");
        assert_eq!(c("dhkt"), "왓");
        assert_eq!(c("dhkT"), "왔");
        assert_eq!(c("dmlrnjs"), "의권");
        assert_eq!(c("ekfrdms"), "닭은");
    }

    #[test]
    fn shift_is_meaningful_only_on_seven_keys() {
        assert_eq!(c("Rk"), "까");
        assert_eq!(c("Tkfkd"), "싸랑");
        assert_eq!(c("dP"), "예");
        assert_eq!(c("dO"), "얘");
        // Shift on keys with no shifted jamo changes nothing: the bug-2 case.
        assert_eq!(c("GKSRMF"), "한끌"); // R is a real shifted key
        assert_eq!(c("DKSSUDGKSKDY"), "안녕하나요");
        assert_eq!(c("DKSSUD"), "안녕");
        assert_eq!(c("AKSGDL"), "많이");
    }

    #[test]
    fn stray_jamo_counted() {
        let r = compose_str("hello");
        assert_eq!(r.text, "ㅗ디ㅣㅐ");
        assert_eq!(r.stray, 3);
        assert_eq!(r.syllables, 1);
        let r = compose_str("rr");
        assert_eq!(r.text, "ㄱㄱ");
        assert_eq!(r.stray, 2);
    }

    #[test]
    fn lone_consonant_cluster_is_one_char() {
        let r = compose_str("qt");
        assert_eq!(r.text, "ㅄ");
        assert_eq!(r.stray, 1);
        assert_eq!(c("rtk"), "ㄱ사");
        assert_eq!(c("rr"), "ㄱㄱ");
        assert_eq!(c("qtqt"), "ㅄㅄ");
    }

    #[test]
    fn double_consonant_cannot_be_final() {
        assert_eq!(c("dkE"), "아ㄸ");
        assert_eq!(c("dkEk"), "아따");
    }

    #[test]
    fn to_keys_roundtrip() {
        assert_eq!(to_keys("안녕하세요").unwrap(), "dkssudgktpdy");
        assert_eq!(to_keys("왔").unwrap(), "dhkT");
        assert_eq!(to_keys("닭").unwrap(), "ekfr");
        for w in ["값이", "의권", "뷁", "쀍", "예쁘다", "괜찮아"] {
            assert_eq!(compose_str(&to_keys(w).unwrap()).text, w);
        }
    }

    #[test]
    fn punctuation_passes_through() {
        assert_eq!(c("dkssud."), "안녕.");
        assert_eq!(c("dk?"), "아?");
    }
}
