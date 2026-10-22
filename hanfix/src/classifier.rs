//! Decides whether a finished word was typed in the wrong Korean/English mode.
//!
//! Conservative by design: when unsure, keep. Shift is evaluated per key the way the
//! Korean IME does, so capitals typed in Hangul mode are never treated as English evidence.

use crate::dubeolsik::{self, Composed};
use crate::lexicon::Lexicon;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Hangul,
    English,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Keep,
    /// Replace the word with this Korean text and switch the IME to Hangul.
    ToKorean(String),
    /// Replace the word with this English text and switch the IME to English.
    ToEnglish(String),
    /// English word + Korean particle typed in one mode (`html로` as `htmlfh` or `ㅗ싀로`):
    /// replace the word with this text and continue in Hangul mode.
    Mixed(String),
}

/// What is known about the words around the one being judged.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Context {
    /// Language of the previous word in this sentence: `Some(true)` = Korean.
    pub prev_korean: Option<bool>,
}

/// A one-syllable English-mode word may turn Korean (with Korean before it) only if its English
/// reading is rarer than this. Real two-letter English words (is, to, at, it) are far above; the
/// subtitle noise that collides with common syllables (dl 이, dk 아, ek 다, tn 수) is far below.
pub const EN_SHORT_KEEP: u32 = 5000;

/// An English word this frequent is never converted away from English.
pub const EN_KEEP: u32 = 300;
/// A Hangul-mode word converts to English only if the English reading is at least this frequent.
pub const EN_CONVERT: u32 = 2000;
/// A one-syllable Korean reading must be at least this frequent (`dk` -> 아 is too ambiguous otherwise).
pub const KO_ONE_SYLLABLE: u32 = 2000;
/// Weakest-syllable familiarity (log10 corpus count) an unlisted Korean reading needs before a
/// Hangul-mode word is trusted as Korean as typed.
pub const KO_FAMILIAR: f32 = 3.5;

/// Tunable decision thresholds (measured with `cargo run --release --example sweep`).
#[derive(Debug, Clone, Copy)]
pub struct Thresholds {
    /// English mode: weakest-syllable familiarity an unlisted Korean reading needs to win against
    /// an English reading that is not a known English word.
    pub ko_familiar_vs_unknown_english: f32,
    /// Mid-word conversion: familiarity the finished syllables need (stricter: no Space to wait for).
    pub live_ko_familiar: f32,
    /// Mid-word conversion waits for Space if the letters start with an English word this frequent.
    pub live_stem_count: u32,
    /// One-syllable English-mode words turn Korean only right after a Korean word.
    pub short_needs_korean_context: bool,
    /// English word + Korean word written together (`tab바`, `for문`): the Korean part must be a
    /// Korean word at least this frequent (particles always qualify). `u32::MAX` = particles only.
    pub mixed_korean_word_min: u32,
    /// English word + one of [`MIXED_NOUNS`] written together.
    pub mixed_nouns: bool,
}

impl Default for Thresholds {
    fn default() -> Self {
        Thresholds {
            ko_familiar_vs_unknown_english: 1.5,
            live_ko_familiar: 4.5,
            live_stem_count: 300,
            // Measured on the tuning half: no extra false conversions without the context.
            short_needs_korean_context: false,
            mixed_korean_word_min: u32::MAX,
            // Curated attached nouns: rare-English false conversions 1.42% -> 1.46% on the tuning half.
            mixed_nouns: true,
        }
    }
}

/// Text the word shows on screen when typed in `mode`.
pub fn screen_text(keys: &[(char, bool)], mode: Mode) -> String {
    match mode {
        Mode::Hangul => dubeolsik::compose(keys).text,
        Mode::English => keys.iter().map(|&(k, s)| if s { k.to_ascii_uppercase() } else { k }).collect(),
    }
}

fn korean_is_plausible(lex: &Lexicon, h: &Composed) -> bool {
    h.stray == 0 && h.syllables > 0 && (lex.ko_count(&h.text) > 0 || lex.ko_familiarity(&h.text) >= KO_FAMILIAR)
}

/// Jamo-only chat tokens (ㅋㅋㅋ, ㅠㅠ, ㅇㅇ, ㄱㅅ) are deliberate Korean, never misspelled English.
fn is_jamo_chat(h: &Composed) -> bool {
    if h.syllables > 0 {
        return false;
    }
    let all_consonants = h.text.chars().all(|c| ('ㄱ'..='ㅎ').contains(&c));
    let mut jamo: Vec<char> = h.text.chars().collect();
    jamo.dedup();
    all_consonants || jamo.len() == 1
}

/// Judge one word. `keys` are the presses since the last boundary: `(lowercase key, shift)`.
/// Trailing punctuation is allowed; anything else that is not a letter keeps the word as typed.
pub fn judge(keys: &[(char, bool)], mode: Mode) -> Verdict {
    judge_with(keys, mode, &Thresholds::default())
}

pub fn judge_with(keys: &[(char, bool)], mode: Mode, t: &Thresholds) -> Verdict {
    judge_ctx(keys, mode, t, Context::default())
}

/// Particles that attach to an English noun (`html로`, `github에서`). Sentence endings like 요/야
/// are left out: English names end in the same keys (goldy, bandi).
const MIXED_PARTICLES: [&str; 22] = [
    "은", "는", "이", "가", "을", "를", "에", "의", "도", "로", "으로", "와", "과", "만", "에서", "에게", "까지",
    "부터", "처럼", "보다", "이랑", "랑",
];

/// One-syllable particles a lone two-letter English-mode word may become without Korean context.
const SHORT_PARTICLES: [&str; 13] = ["이", "가", "은", "는", "을", "를", "에", "의", "도", "로", "와", "과", "만"];

/// Korean nouns commonly written right after an English word (`tab바`, `for문`, `config파일`).
/// Origin: 바/문 from user reports (2026-10-22: `ㅅ뷰바`, for문 from Catchtype's examples); the rest
/// are common UI/dev nouns picked by hand. Re-grade the held-out gates when adding entries.
const MIXED_NOUNS: [&str; 24] = [
    "바", "문", "창", "키", "칸", "앱", "값", "식", "형", "용", "별", "들", "님", "씨", "쪽", "파일", "버튼", "코드",
    "서버", "폴더", "메뉴", "화면", "기능", "모드",
];

/// English word + Korean particle in one word: `htmlfh` / `ㅗ싀로` -> `html로`.
/// The longest known-English prefix whose remaining keys spell exactly a particle.
fn mixed(core: &[(char, bool)], tail_text: &str, mode: Mode, lex: &Lexicon, t: &Thresholds) -> Option<String> {
    mixed_split(core, tail_text, mode, lex, t).map(|(text, _)| text)
}

/// [`mixed`] plus the length of the English part.
fn mixed_split(core: &[(char, bool)], tail_text: &str, mode: Mode, lex: &Lexicon, t: &Thresholds) -> Option<(String, usize)> {
    (2..core.len()).rev().find_map(|i| {
        let (eng_keys, kor_keys) = core.split_at(i);
        let eng: String = eng_keys.iter().map(|(k, _)| k.to_ascii_lowercase()).collect();
        // A real English word (spelling dictionary), not subtitle noise like `qu`, `rne`.
        if eng.len() < 3 || !lex.en_in_dictionary(&eng) {
            return None;
        }
        let kor = dubeolsik::compose(kor_keys);
        let korean_word = t.mixed_korean_word_min != u32::MAX && kor.syllables > 0 && lex.ko_count(&kor.text) >= t.mixed_korean_word_min;
        let attached_noun = t.mixed_nouns && MIXED_NOUNS.contains(&kor.text.as_str());
        if kor.stray > 0 || !(MIXED_PARTICLES.contains(&kor.text.as_str()) || korean_word || attached_noun) {
            return None;
        }
        // In Hangul mode the English part must be visibly not Korean (ㅗ싀 for html).
        if mode == Mode::Hangul && dubeolsik::compose(eng_keys).stray == 0 {
            return None;
        }
        let english: String = eng_keys.iter().map(|&(k, s)| if s { k.to_ascii_uppercase() } else { k }).collect();
        Some((english + &kor.text + tail_text, i))
    })
}

/// [`judge_with`] plus what the surrounding words say (see [`Context`]).
pub fn judge_ctx(keys: &[(char, bool)], mode: Mode, t: &Thresholds, ctx: Context) -> Verdict {
    let core_len = keys.iter().rposition(|(k, _)| k.is_ascii_alphabetic()).map_or(0, |i| i + 1);
    let (core, tail) = keys.split_at(core_len);
    if core.len() < 2 || !core.iter().all(|(k, _)| k.is_ascii_alphabetic()) {
        return Verdict::Keep;
    }
    if !tail.iter().all(|(k, _)| matches!(k, '.' | ',' | '?' | '!' | ';' | ':' | '\'' | '"' | ')')) {
        return Verdict::Keep;
    }
    let tail_text: String = tail.iter().map(|(k, _)| *k).collect();
    let lex = Lexicon::get();
    let lower: String = core.iter().map(|(k, _)| k.to_ascii_lowercase()).collect();
    let hangul = dubeolsik::compose(core);

    match mode {
        Mode::English => {
            let all_caps = core.iter().all(|&(_, s)| s);
            if all_caps && core.len() <= 5 {
                return Verdict::Keep; // acronym: TV, API
            }
            // (A two-letter word after a Korean word is weighed below instead: dl after 이 is 이.)
            // Without a Korean word before it, only a particle reading (table dl -> table 이):
            // other syllables collide with short abbreviations (db 유, cm 츠, sh 노).
            let particle = SHORT_PARTICLES.contains(&hangul.text.as_str());
            let context_ok = ctx.prev_korean == Some(true) || (!t.short_needs_korean_context && particle);
            let short_after_korean = core.len() == 2 && context_ok && lex.en_count(&lower) < EN_SHORT_KEEP;
            if lex.en_count(&lower) >= EN_KEEP && !short_after_korean {
                return Verdict::Keep;
            }
            if hangul.stray > 0 || hangul.syllables == 0 {
                // Maybe English + a Korean particle (htmlfh -> html로), if the whole is not English.
                if !lex.en_known(&lower) {
                    if let Some(text) = mixed(core, &tail_text, mode, lex, t) {
                        return Verdict::Mixed(text);
                    }
                }
                return Verdict::Keep;
            }
            // A listed (if rare) English word needs a listed Korean reading to lose; one syllable needs a common one.
            let ko = lex.ko_count(&hangul.text);
            let english_listed = lex.en_known(&lower);
            if ko == 0 && lex.ko_familiarity(&hangul.text) < t.ko_familiar_vs_unknown_english {
                return Verdict::Keep;
            }
            // One syllable after a Korean word: the common syllable wins over rare English noise.
            let short_in_korean = hangul.syllables == 1
                && context_ok
                && ko >= KO_ONE_SYLLABLE
                && lex.en_count(&lower) < EN_SHORT_KEEP;
            if !short_in_korean
                && ((english_listed && (ko == 0 || hangul.syllables == 1)) || (hangul.syllables == 1 && ko < KO_ONE_SYLLABLE))
            {
                return Verdict::Keep;
            }
            // `forans` reads as 랙문 (no Korean word) but is for + 문: prefer the English split.
            // An unlisted Korean reading that also splits as English word + noun/particle:
            // - long English part (config파일, class문): the split
            // - odd Korean reading (forans 랙문 / for문, 햇님 / got님, 냇가 / sot가): genuinely
            //   ambiguous, leave it as typed rather than produce a wrong word either way
            // - familiar Korean reading (행님, 생식): Korean, as before
            if ko == 0 {
                if let Some((text, eng_len)) = mixed_split(core, &tail_text, mode, lex, t) {
                    if eng_len >= 4 {
                        return Verdict::Mixed(text);
                    }
                    if lex.ko_familiarity(&hangul.text) < KO_FAMILIAR {
                        return Verdict::Keep;
                    }
                }
            }
            Verdict::ToKorean(hangul.text + &tail_text)
        }
        Mode::Hangul => {
            let english = || core.iter().map(|&(k, s)| if s { k.to_ascii_uppercase() } else { k }).collect::<String>();
            // A known English word whose Hangul reading is no known Korean word (녀애 = sudo).
            // Measured against 13,900 rare Korean words: the only collisions were names (toru,
            // shan, theo), which the spelling dictionary does not contain.
            // Spelling-dictionary words only: names in subtitles (toru = 새겨) must not win.
            if lower.len() >= 3 && hangul.syllables > 0 && lex.ko_count(&hangul.text) == 0 && lex.en_in_dictionary(&lower) {
                return Verdict::ToEnglish(english() + &tail_text);
            }
            // What the user saw is the IME output; capitals are not evidence of anything here.
            if korean_is_plausible(lex, &hangul) || is_jamo_chat(&hangul) {
                return Verdict::Keep;
            }
            // A frequent English word, or any known English word whose Hangul reading has stray jamo
            // (`sync` -> 뇨ㅜㅊ cannot be Korean as typed; a correctly typed Korean word never has them).
            // Two letters only for real short words (is, it, my, ok), not subtitle noise.
            let known = lex.en_known(&lower) && (lower.len() >= 3 || lex.en_count(&lower) >= EN_SHORT_KEEP);
            if lex.en_count(&lower) >= EN_CONVERT || (hangul.stray > 0 && known) {
                return Verdict::ToEnglish(english() + &tail_text);
            }
            mixed(core, &tail_text, mode, lex, t).map_or(Verdict::Keep, Verdict::Mixed)
        }
    }
}

/// A correction to apply while the word is still being typed (English mode -> Korean).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveFix {
    /// Finished syllables to put on screen in place of the typed letters.
    pub committed: String,
    /// Keys from this index on are replayed through the Korean IME to rebuild the syllable still being typed.
    pub replay_from: usize,
}

fn is_syllable(c: char) -> bool {
    ('\u{AC00}'..='\u{D7A3}').contains(&c)
}

/// Mid-word check after each letter typed in English mode (the "convert while typing" speed-up).
/// Stricter than [`judge`], which still runs at Space for anything this lets through:
/// - the Korean reading has 2+ syllables and every character but the last is a finished syllable
/// - no known English word starts with the letters typed so far (`goth` could become `gothic`)
/// - the finished syllables are a known Korean word or made of common syllables
/// Mid-word check after each key typed in Hangul mode: the English text to switch to now.
///
/// Normally Hangul-mode words are judged at Space, but some apps (terminals) never let the IME see
/// the Space, so a strong case is fixed while typing: 4+ letters, the Hangul reading has stray
/// jamo (cannot be Korean as typed: `best` -> ㅠㄷㄴㅅ), and the English reading is a frequent word.
/// If the word goes on (`hell` of `hello`), the rest is typed in English mode anyway.
pub fn live_reverse(keys: &[(char, bool)]) -> Option<String> {
    if keys.len() < 4 || !keys.iter().all(|(k, _)| k.is_ascii_alphabetic()) {
        return None;
    }
    let lex = Lexicon::get();
    let h = dubeolsik::compose(keys);
    if h.stray == 0 || is_jamo_chat(&h) {
        return None;
    }
    // A lone last jamo is just the next syllable being started (잭ㅇ while typing 잭이야 = "word"):
    // only stray jamo before the last character prove the word cannot be Korean.
    let chars: Vec<char> = h.text.chars().collect();
    let stray_before_last = chars[..chars.len() - 1].iter().any(|&c| ('\u{3131}'..='\u{318E}').contains(&c));
    if !stray_before_last {
        return None;
    }
    let lower: String = keys.iter().map(|(k, _)| k.to_ascii_lowercase()).collect();
    if lex.en_count(&lower) < EN_CONVERT {
        return None;
    }
    Some(keys.iter().map(|&(k, s)| if s { k.to_ascii_uppercase() } else { k }).collect())
}

pub fn live_forward(keys: &[(char, bool)], t: &Thresholds) -> Option<LiveFix> {
    if keys.len() < 3 || !keys.iter().all(|(k, _)| k.is_ascii_alphabetic()) {
        return None;
    }
    // Korean needs Shift only on q w e r t o p; a capital anywhere else is English (CamelCase, caps).
    if keys.iter().any(|&(k, s)| s && !dubeolsik::SHIFTABLE.contains(&k.to_ascii_lowercase())) {
        return None;
    }
    let lex = Lexicon::get();
    let lower: String = keys.iter().map(|(k, _)| k.to_ascii_lowercase()).collect();
    if lex.en_has_prefix(&lower) {
        return None;
    }
    // Compounds and inflections of a common word (today·s, and·over, rock·away) fail the prefix
    // test only near the end; leave them to the check at Space.
    if (3..lower.len()).any(|n| lex.en_count(&lower[..n]) >= t.live_stem_count) {
        return None;
    }
    let h = dubeolsik::compose(keys);
    let chars: Vec<char> = h.text.chars().collect();
    let finished = chars.iter().filter(|&&c| is_syllable(c)).count();
    if finished < 2 || chars[..chars.len() - 1].iter().any(|&c| !is_syllable(c)) {
        return None;
    }
    let committed: String = chars[..chars.len() - 1].iter().collect();
    if lex.ko_count(&committed) == 0 && lex.ko_familiarity(&committed) < t.live_ko_familiar {
        return None;
    }
    // The last character is rebuilt by the IME: find where its keys start.
    let replay_from = (1..keys.len()).rev().find(|&i| dubeolsik::compose(&keys[..i]).text == committed)?;
    Some(LiveFix { committed, replay_from })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(s: &str) -> Vec<(char, bool)> {
        s.chars().map(|c| (c.to_ascii_lowercase(), c.is_ascii_uppercase())).collect()
    }

    fn j(s: &str, mode: Mode) -> Verdict {
        judge(&keys(s), mode)
    }

    #[test]
    fn english_mode_korean_gets_converted() {
        assert_eq!(j("dkssudgktpdy", Mode::English), Verdict::ToKorean("안녕하세요".into()));
        assert_eq!(j("rkatkgkqslek", Mode::English), Verdict::ToKorean("감사합니다".into()));
        assert_eq!(j("dkssud.", Mode::English), Verdict::ToKorean("안녕.".into()));
        assert_eq!(j("tkfkd", Mode::English), Verdict::ToKorean("사랑".into()));
    }

    #[test]
    fn hangul_mode_english_gets_converted() {
        assert_eq!(j("hello", Mode::Hangul), Verdict::ToEnglish("hello".into()));
        assert_eq!(j("Hello", Mode::Hangul), Verdict::ToEnglish("Hello".into()));
        assert_eq!(j("sync", Mode::Hangul), Verdict::ToEnglish("sync".into()), "뇨ㅜㅊ has stray jamo");
    }

    /// Bug 2: Shift held while typing Korean on keys without a shifted jamo.
    #[test]
    fn shift_held_korean_is_never_converted_to_english() {
        for w in ["DKSSUDGKSKDY", "GKSRMF", "AKSGDL", "DKSSUD", "SKSMS", "DLFKDHK", "GKFRK", "DLTSK", "AJGO", "GKSKA"] {
            assert_eq!(j(w, Mode::Hangul), Verdict::Keep, "{w}");
        }
        // Mixed: some capitals, some not.
        for w in ["dKSSUD", "GKsrmf", "aKSGdl"] {
            assert_eq!(j(w, Mode::Hangul), Verdict::Keep, "{w}");
        }
    }

    #[test]
    fn user_reported_words() {
        // 녀애 is no Korean word, sudo is English.
        assert_eq!(j("sudo", Mode::Hangul), Verdict::ToEnglish("sudo".into()));
        // English word + particle, typed in either mode.
        assert_eq!(j("htmlfh", Mode::Hangul), Verdict::Mixed("html로".into()));
        assert_eq!(j("htmlfh", Mode::English), Verdict::Mixed("html로".into()));
        assert_eq!(j("githubdp", Mode::English), Verdict::Mixed("github에".into()));
        // Names ending in particle keys stay as typed (요/야 are not mixed particles).
        for name in ["goldy", "bandi", "wendi", "kody"] {
            assert_eq!(j(name, Mode::English), Verdict::Keep, "{name}");
        }
        // One syllable: only with a Korean word before it.
        let ctx = Context { prev_korean: Some(true) };
        let t = Thresholds::default();
        assert_eq!(judge_ctx(&keys("dl"), Mode::English, &t, ctx), Verdict::ToKorean("이".into()));
        assert_eq!(j("dl", Mode::English), Verdict::ToKorean("이".into()), "also without a Korean word before it (table dl)");
        // Short real English typed in Hangul mode (ㅑㄴ), English + attached Korean noun.
        assert_eq!(j("is", Mode::Hangul), Verdict::ToEnglish("is".into()));
        assert_eq!(j("tabqk", Mode::Hangul), Verdict::Mixed("tab바".into()));
        assert_eq!(j("tabqk", Mode::English), Verdict::Mixed("tab바".into()));
        assert_eq!(j("forans", Mode::English), Verdict::Keep, "for문 vs 랙문: ambiguous, left as typed");
        // Korean words typed in English mode stay whole Korean words (review: 행님 != god님).
        for w in ["행님", "생식", "생도", "생의", "행에"] {
            let k = crate::dubeolsik::to_keys(w).unwrap();
            assert_eq!(j(&k, Mode::English), Verdict::ToKorean(w.into()), "{w}");
        }
        // Ambiguous (odd Korean reading that is also English + noun): left as typed, never got님/sot가.
        for w in ["햇님", "냇가", "냉이"] {
            let k = crate::dubeolsik::to_keys(w).unwrap();
            assert!(!matches!(j(&k, Mode::English), Verdict::Mixed(_)), "{w}");
        }
        // Lone abbreviations are not turned into syllables without Korean before them.
        for abbr in ["db", "dm", "cm", "cu", "ep", "dj", "sh", "fm"] {
            assert_eq!(j(abbr, Mode::English), Verdict::Keep, "{abbr}");
        }
        // Chat jamo stay: consonants only or one repeated jamo.
        // KakaoTalk-style jamo (ㅋㅋㅠㅠ ㅠㅜ ㅇㅋㅎ ㅎㅎㅎ ㅇㅈ ㄹㅇ ㅇㄷ ㅜㅠ ㅠㅠㅠ ㅋㅋㅋㅋ ㅎㅇ ㄱㄱ ㅃㅇ ㅂㅂ ㄴㄴ ㅇㅋ).
        for chat in ["zzbb", "bn", "dzg", "ggg", "dw", "fd", "de", "nb", "bbb", "zzzz", "gd", "rr", "Qd", "qq", "ss", "dz"] {
            assert_eq!(j(chat, Mode::Hangul), Verdict::Keep, "{chat}");
        }
        for chat in ["zzz", "dd", "rt", "bb", "nn"] {
            assert_eq!(j(chat, Mode::Hangul), Verdict::Keep, "{chat}");
        }
        for real in ["is", "to", "at", "it", "on", "my"] {
            assert_eq!(judge_ctx(&keys(real), Mode::English, &t, ctx), Verdict::Keep, "{real}");
        }
    }

    #[test]
    fn jamo_chat_tokens_are_kept() {
        for w in ["zzz", "zzzz", "bb", "gg", "dd", "ss", "rt", "dw", "gd", "tr", "qb"] {
            assert_eq!(j(w, Mode::Hangul), Verdict::Keep, "{w}");
        }
        assert_eq!(j("bye", Mode::Hangul), Verdict::ToEnglish("bye".into()));
    }

    #[test]
    fn correct_words_are_kept() {
        for w in ["hello", "the", "github", "TV", "API", "v2", "build.sh", "rust"] {
            assert_eq!(j(w, Mode::English), Verdict::Keep, "{w}");
        }
        for w in ["dkssud", "gksrmf", "tkfkd"] {
            assert_eq!(j(w, Mode::Hangul), Verdict::Keep, "{w}");
        }
    }

    /// Corpus gate (R10): no word typed in its correct mode may be converted, and report recall.
    #[test]
    fn corpus_false_conversion_gate() {
        let lex_en = include_str!("../data/en.txt");
        let lex_ko = include_str!("../data/ko.txt");
        let mut en_fp = Vec::new();
        let (mut en_total, mut en_recall, mut en_frequent) = (0, 0, 0);
        for line in lex_en.lines() {
            let (w, c) = line.split_once(' ').unwrap();
            let c: u32 = c.parse().unwrap();
            if w.len() < 2 {
                continue;
            }
            en_total += 1;
            if j(w, Mode::English) != Verdict::Keep {
                en_fp.push((w.to_string(), c));
            }
            en_frequent += (c >= EN_CONVERT) as u32;
            if c >= EN_CONVERT && matches!(j(w, Mode::Hangul), Verdict::ToEnglish(_)) {
                en_recall += 1;
            }
        }
        let mut ko_fp = Vec::new();
        let (mut ko_total, mut ko_recall) = (0, 0);
        for line in lex_ko.lines() {
            let (w, _) = line.split_once(' ').unwrap();
            if w.chars().count() < 2 {
                continue;
            }
            let k = dubeolsik::to_keys(w).unwrap();
            ko_total += 1;
            if j(&k, Mode::Hangul) != Verdict::Keep {
                ko_fp.push(w.to_string());
            }
            if matches!(j(&k, Mode::English), Verdict::ToKorean(_)) {
                ko_recall += 1;
            }
            // Bug 2: same word with Shift held on every key that has no shifted jamo.
            let held: String = k
                .chars()
                .map(|c| if dubeolsik::SHIFTABLE.contains(&c) { c } else { c.to_ascii_uppercase() })
                .collect();
            if j(&held, Mode::Hangul) != Verdict::Keep {
                ko_fp.push(format!("{w} (shift held)"));
            }
        }
        en_fp.sort_by(|a, b| b.1.cmp(&a.1));
        println!("EN typed in English, wrongly converted: {} / {en_total}; top: {:?}", en_fp.len(), &en_fp[..en_fp.len().min(40)]);
        println!("KO typed in Hangul, wrongly converted: {} / {ko_total}; sample: {:?}", ko_fp.len(), &ko_fp[..ko_fp.len().min(40)]);
        println!("recall: KO typed in English -> Korean {ko_recall}/{ko_total}; frequent EN typed in Hangul -> English {en_recall}/{en_frequent}");
        assert!(ko_fp.is_empty(), "Korean words typed correctly must never convert");
        let rate = en_fp.len() as f64 / en_total as f64;
        assert!(rate < 0.002, "English false conversion rate {rate}");
    }
}
