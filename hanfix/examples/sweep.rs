//! Threshold sweep on the held-out sets: recall vs. false conversions.
//! `cargo run --release --example sweep -- <unim english_words.txt>` (optional: words unseen by both dictionaries)
use hanfix::classifier::{judge_with, Mode, Thresholds, Verdict};
use hanfix::dubeolsik::{to_keys, SHIFTABLE};
use hanfix::lexicon::Lexicon;

fn keys(s: &str) -> Vec<(char, bool)> {
    s.chars().map(|c| (c.to_ascii_lowercase(), c.is_ascii_uppercase())).collect()
}

fn main() {
    let lex = Lexicon::get();
    let ko: Vec<(String, String)> = include_str!("../tests/data/ko_heldout.txt")
        .lines()
        .filter(|l| !l.is_empty())
        .step_by(2) // tuning half: even lines (tests/heldout.rs grades the odd lines)
        .filter_map(|w| Some((w.to_string(), to_keys(w)?)))
        .collect();
    let en: Vec<&str> = include_str!("../tests/data/en_heldout.txt").lines().filter(|l| !l.is_empty()).step_by(2).collect();
    let unseen: Vec<&str> = en.iter().copied().filter(|w| !lex.en_known(w)).collect();
    let ids: Vec<&str> = include_str!("../tests/data/identifiers.txt").lines().filter(|l| !l.is_empty()).step_by(2).collect();
    let pct = |a: usize, b: usize| format!("{a:>5}/{b} ({:>5.2}%)", 100.0 * a as f64 / b as f64);
    for f in [3.5, 3.0, 2.5, 2.0, 1.5, 1.0, 0.0] {
        let t = Thresholds { ko_familiar_vs_unknown_english: f, ..Thresholds::default() };
        let recall = ko.iter().filter(|(_, k)| matches!(judge_with(&keys(k), Mode::English, &t), Verdict::ToKorean(_))).count();
        let ko_wrong = ko
            .iter()
            .filter(|(_, k)| {
                let held: String = k.chars().map(|c| if SHIFTABLE.contains(&c) { c } else { c.to_ascii_uppercase() }).collect();
                judge_with(&keys(k), Mode::Hangul, &t) != Verdict::Keep || judge_with(&keys(&held), Mode::Hangul, &t) != Verdict::Keep
            })
            .count();
        let en_wrong = en.iter().filter(|w| judge_with(&keys(w), Mode::English, &t) != Verdict::Keep).count();
        let unseen_wrong = unseen.iter().filter(|w| judge_with(&keys(w), Mode::English, &t) != Verdict::Keep).count();
        let id_wrong = ids.iter().filter(|w| judge_with(&keys(w), Mode::English, &t) != Verdict::Keep).count();
        println!(
            "fam {f:>3}: KO recall {} | KO kept wrong {} | rare EN wrong {} | EN unseen by both dicts wrong {} | identifiers wrong {}",
            pct(recall, ko.len()),
            pct(ko_wrong, ko.len()),
            pct(en_wrong, en.len()),
            pct(unseen_wrong, unseen.len()),
            pct(id_wrong, ids.len())
        );
    }
}
