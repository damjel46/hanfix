//! Tuning half (even lines) only: live + Space combined, per word, for a grid of live settings.
use hanfix::classifier::{judge_with, live_forward, Mode, Thresholds, Verdict};
use hanfix::dubeolsik::to_keys;

fn keys(s: &str) -> Vec<(char, bool)> {
    s.chars().map(|c| (c.to_ascii_lowercase(), c.is_ascii_uppercase())).collect()
}

fn even(data: &'static str) -> Vec<&'static str> {
    data.lines().filter(|l| !l.is_empty()).step_by(2).collect()
}

fn main() {
    let ko: Vec<String> = even(include_str!("../tests/data/ko_heldout.txt")).into_iter().filter_map(to_keys).collect();
    let en = even(include_str!("../tests/data/en_heldout.txt"));
    let ids = even(include_str!("../tests/data/identifiers.txt"));
    let pct = |a: usize, b: usize| format!("{:>5.2}%", 100.0 * a as f64 / b as f64);
    for (live_fam, stem) in [(3.0, 1000), (3.5, 1000), (4.0, 1000), (3.0, 300), (4.0, 300), (4.5, 300), (5.0, 300)] {
        let t = Thresholds { live_ko_familiar: live_fam, live_stem_count: stem, ..Thresholds::default() };
        let live = |k: &[(char, bool)]| (1..=k.len()).find(|&n| live_forward(&k[..n], &t).is_some());
        let space = |k: &[(char, bool)]| judge_with(k, Mode::English, &t) != Verdict::Keep;
        let ko_live = ko.iter().filter(|w| live(&keys(w)).is_some()).count();
        let ko_any = ko.iter().filter(|w| live(&keys(w)).is_some() || space(&keys(w))).count();
        let en_live = en.iter().filter(|w| live(&keys(w)).is_some()).count();
        let en_any = en.iter().filter(|w| live(&keys(w)).is_some() || space(&keys(w))).count();
        let id_any = ids.iter().filter(|w| live(&keys(w)).is_some() || space(&keys(w))).count();
        println!(
            "live fam {live_fam} stem {stem}: KO live {} / any {} | rare EN live {} / any {} | identifiers any {}",
            pct(ko_live, ko.len()),
            pct(ko_any, ko.len()),
            pct(en_live, en.len()),
            pct(en_any, en.len()),
            pct(id_any, ids.len())
        );
    }
}
