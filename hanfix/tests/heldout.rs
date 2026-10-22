//! Held-out false-conversion gate (R10b): words the classifier's lexicon has never seen.
//!
//! - English ranks 50,001+ in FrequencyWords (rare words, names), typed in English mode
//! - Purely alphabetic identifiers from real Rust source, typed in English mode
//! - Korean ranks 50,001+ (rare words, names, slang), typed in Hangul mode, with and without Shift held
//!
//! Thresholds are tuned on the even lines (`examples/sweep.rs`, `examples/live_sweep.rs`); this gate
//! only looks at the odd lines, so its numbers are not graded on the data they were tuned on.
//! A word counts as wrongly converted if either the mid-word (live) rule fires on any prefix or the
//! check at Space converts it.

use hanfix::classifier::{judge, live_forward, Mode, Thresholds, Verdict};
use hanfix::dubeolsik::{to_keys, SHIFTABLE};

fn keys(s: &str) -> Vec<(char, bool)> {
    s.chars().map(|c| (c.to_ascii_lowercase(), c.is_ascii_uppercase())).collect()
}

/// The test half: odd lines.
fn load(data: &str) -> Vec<String> {
    data.lines().filter(|l| !l.is_empty()).skip(1).step_by(2).map(str::to_string).collect()
}

fn rate(name: &str, total: usize, wrong: &[String]) -> f64 {
    let r = wrong.len() as f64 / total as f64;
    println!("{name}: {} / {total} wrongly converted ({:.3}%) e.g. {:?}", wrong.len(), r * 100.0, &wrong[..wrong.len().min(20)]);
    r
}

/// Typed in English mode: converted mid-word or at Space?
fn converted_in_english_mode(word: &str) -> bool {
    let k = keys(word);
    let t = Thresholds::default();
    (1..=k.len()).any(|n| live_forward(&k[..n], &t).is_some()) || judge(&k, Mode::English) != Verdict::Keep
}

#[test]
fn heldout_english_words() {
    let words = load(include_str!("data/en_heldout.txt"));
    let wrong: Vec<String> = words.iter().filter(|w| converted_in_english_mode(w)).cloned().collect();
    // Catchtype's engine converts 1.7% of this kind of word (measured on the same source).
    assert!(rate("held-out English (EN mode, live + Space)", words.len(), &wrong) < 0.015);
}

#[test]
fn heldout_code_identifiers() {
    let words = load(include_str!("data/identifiers.txt"));
    let wrong: Vec<String> = words.iter().filter(|w| converted_in_english_mode(w)).cloned().collect();
    assert!(rate("code identifiers (EN mode, live + Space)", words.len(), &wrong) < 0.006);
}

#[test]
fn heldout_korean_words_with_and_without_shift() {
    let words = load(include_str!("data/ko_heldout.txt"));
    let mut wrong = Vec::new();
    for w in &words {
        let k = to_keys(w).unwrap();
        let held: String = k.chars().map(|c| if SHIFTABLE.contains(&c) { c } else { c.to_ascii_uppercase() }).collect();
        if judge(&keys(&k), Mode::Hangul) != Verdict::Keep {
            wrong.push(w.clone());
        }
        if judge(&keys(&held), Mode::Hangul) != Verdict::Keep {
            wrong.push(format!("{w}(shift)"));
        }
    }
    assert!(rate("held-out Korean (HAN mode, x2 with Shift held)", words.len(), &wrong) < 0.001);
    let recalled = words
        .iter()
        .filter(|w| {
            let k = keys(&to_keys(w).unwrap());
            let t = Thresholds::default();
            (1..=k.len()).any(|n| live_forward(&k[..n], &t).is_some()) || judge(&k, Mode::English) == Verdict::ToKorean(w.to_string())
        })
        .count();
    println!("held-out Korean typed in EN mode -> converted (live or Space): {recalled} / {}", words.len());
    assert!(recalled as f64 / words.len() as f64 > 0.98, "recall regressed");
}

/// The same gates with context: every word typed right after a Korean word (the normal case when
/// mixing languages). The one-syllable rule only acts with this context, so it is graded here.
#[test]
fn heldout_with_korean_before() {
    use hanfix::classifier::{judge_ctx, Context};
    let t = Thresholds::default();
    let after_korean = Context { prev_korean: Some(true) };
    let converted = |w: &str, ctx| {
        let k = keys(w);
        (1..=k.len()).any(|n| live_forward(&k[..n], &t).is_some()) || judge_ctx(&k, Mode::English, &t, ctx) != Verdict::Keep
    };
    let en = load(include_str!("data/en_heldout.txt"));
    let wrong: Vec<String> = en.iter().filter(|w| converted(w, after_korean)).cloned().collect();
    assert!(rate("held-out English after a Korean word", en.len(), &wrong) < 0.015);
    let ids = load(include_str!("data/identifiers.txt"));
    let wrong: Vec<String> = ids.iter().filter(|w| converted(w, after_korean)).cloned().collect();
    assert!(rate("identifiers after a Korean word", ids.len(), &wrong) < 0.006);
    // Real short English words must survive right after Korean.
    let short = ["is", "to", "at", "it", "on", "my", "we", "me", "be", "do", "go", "no", "so", "up", "us", "an", "as", "by", "he", "if", "in", "of", "or", "ok"];
    let wrong: Vec<String> = short.iter().filter(|w| converted(w, after_korean)).map(|w| w.to_string()).collect();
    assert!(wrong.is_empty(), "short English after Korean converted: {wrong:?}");
    // Korean words in Hangul mode stay Korean whatever came before.
    let ko = load(include_str!("data/ko_heldout.txt"));
    for prev in [Some(true), Some(false)] {
        let ctx = Context { prev_korean: prev };
        let wrong: Vec<String> = ko
            .iter()
            .filter(|w| judge_ctx(&keys(&to_keys(w).unwrap()), Mode::Hangul, &t, ctx) != Verdict::Keep)
            .cloned()
            .collect();
        assert!(rate(&format!("held-out Korean, HAN mode, prev={prev:?}"), ko.len(), &wrong) < 0.001);
    }
}

/// Mid-word Hangul -> English (for apps where the IME never sees the Space, e.g. terminals):
/// must never fire on any prefix of a Korean word typed in Hangul mode, with or without Shift.
#[test]
fn live_reverse_never_fires_inside_korean_words() {
    use hanfix::classifier::live_reverse;
    let mut words: Vec<String> = load(include_str!("data/ko_heldout.txt"));
    words.extend(include_str!("../data/ko.txt").lines().filter_map(|l| l.split(' ').next()).map(str::to_string));
    let mut wrong = Vec::new();
    for w in &words {
        let Some(k) = to_keys(w) else { continue };
        let held: String = k.chars().map(|c| if SHIFTABLE.contains(&c) { c } else { c.to_ascii_uppercase() }).collect();
        for typed in [&k, &held] {
            let ks = keys(typed);
            if (1..=ks.len()).any(|n| live_reverse(&ks[..n]).is_some()) {
                wrong.push(w.clone());
                break;
            }
        }
    }
    assert!(rate("Korean words hit by mid-word Hangul->English", words.len(), &wrong) < 0.0005);
    // And it does catch the terminal case.
    assert_eq!(live_reverse(&keys("best")), Some("best".into()));
}
