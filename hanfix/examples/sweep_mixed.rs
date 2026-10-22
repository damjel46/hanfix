//! Tuning half (even lines): short words without context and English+Korean-word mixing.
use hanfix::classifier::{judge_ctx, live_forward, Context, Mode, Thresholds, Verdict};
use hanfix::dubeolsik::to_keys;

fn keys(s: &str) -> Vec<(char, bool)> {
    s.chars().map(|c| (c.to_ascii_lowercase(), c.is_ascii_uppercase())).collect()
}

fn even(data: &'static str) -> Vec<&'static str> {
    data.lines().filter(|l| !l.is_empty()).step_by(2).collect()
}

fn main() {
    let en = even(include_str!("../tests/data/en_heldout.txt"));
    let ids = even(include_str!("../tests/data/identifiers.txt"));
    let ko: Vec<String> = even(include_str!("../tests/data/ko_heldout.txt")).into_iter().filter_map(to_keys).collect();
    let pct = |a: usize, b: usize| format!("{:>5.2}%", 100.0 * a as f64 / b as f64);
    for (ctx_needed, mixed_min, nouns) in [(false, u32::MAX, true)] {
        let t = Thresholds { short_needs_korean_context: ctx_needed, mixed_korean_word_min: mixed_min, mixed_nouns: nouns, ..Thresholds::default() };
        let after_english = Context { prev_korean: Some(false) };
        let en_conv = |w: &str, ctx| {
            let k = keys(w);
            (1..=k.len()).any(|n| live_forward(&k[..n], &t).is_some()) || judge_ctx(&k, Mode::English, &t, ctx) != Verdict::Keep
        };
        let en_wrong = en.iter().filter(|w| en_conv(w, after_english)).count();
        let id_wrong = ids.iter().filter(|w| en_conv(w, after_english)).count();
        let id_examples: Vec<&&str> = ids.iter().filter(|w| en_conv(w, after_english)).take(12).collect();
        let ko_wrong = ko.iter().filter(|k| judge_ctx(&keys(k), Mode::Hangul, &t, after_english) != Verdict::Keep).count();
        let probe = |w: &str, m| format!("{:?}", judge_ctx(&keys(w), m, &t, after_english));
        println!(
            "ctx_needed={ctx_needed:5} mixed_min={mixed_min:>10} nouns={nouns:5}: rare EN {} | ids {} {:?} | KO {} | dl:{} tabqk(HAN):{} tabqk(EN):{} is(HAN):{}",
            pct(en_wrong, en.len()),
            pct(id_wrong, ids.len()),
            id_examples,
            pct(ko_wrong, ko.len()),
            probe("dl", Mode::English),
            probe("tabqk", Mode::Hangul),
            probe("tabqk", Mode::English),
            probe("is", Mode::Hangul)
        );
        let short = ["is", "to", "at", "it", "on", "my", "we", "me", "be", "do", "go", "no", "so", "up", "us", "an", "as", "by", "he", "if", "in", "of", "or", "ok"];
        let bad: Vec<&&str> = short.iter().filter(|w| en_conv(w, after_english)).collect();
        println!("   short English converted: {bad:?}; forfps(EN) {} configvkdlf(EN) {}", probe("forans", Mode::English), probe("configvkdlf", Mode::English));
    }
}
