//! Mid-word ("live") conversion sweep: how often it fires on real Korean typed in English mode,
//! whether the result is right, and how often it fires on English / identifiers (any prefix counts).
use hanfix::classifier::{live_forward, Thresholds};
use hanfix::dubeolsik::{compose, to_keys};

fn keys(s: &str) -> Vec<(char, bool)> {
    s.chars().map(|c| (c.to_ascii_lowercase(), c.is_ascii_uppercase())).collect()
}

/// First prefix length at which the live rule fires.
fn fires(k: &[(char, bool)], t: &Thresholds) -> Option<usize> {
    (1..=k.len()).find(|&n| live_forward(&k[..n], t).is_some())
}

fn main() {
    let ko: Vec<(String, String)> = include_str!("../tests/data/ko_heldout.txt")
        .lines()
        .chain(include_str!("../data/ko.txt").lines().map(|l| l.split(' ').next().unwrap()).take(20000))
        .filter(|l| l.chars().count() >= 2)
        .filter_map(|w| Some((w.to_string(), to_keys(w)?)))
        .collect();
    let en: Vec<&str> = include_str!("../tests/data/en_heldout.txt")
        .lines()
        .chain(include_str!("../data/en.txt").lines().map(|l| l.split(' ').next().unwrap()))
        .filter(|l| l.len() >= 2)
        .collect();
    let ids: Vec<&str> = include_str!("../tests/data/identifiers.txt").lines().filter(|l| !l.is_empty()).collect();
    let pct = |a: usize, b: usize| format!("{a:>5}/{b} ({:>5.2}%)", 100.0 * a as f64 / b as f64);
    for (f, stem) in [(3.0, u32::MAX), (3.0, 3000), (3.0, 1000), (3.0, 300), (2.0, 1000)] {
        let t = Thresholds { live_ko_familiar: f, live_stem_count: stem, ..Thresholds::default() };
        let (mut live, mut wrong_result) = (0, Vec::new());
        for (w, k) in &ko {
            let k = keys(k);
            if let Some(n) = fires(&k, &t) {
                live += 1;
                // After the fix the rest is typed through the Korean IME.
                let fix = live_forward(&k[..n], &t).unwrap();
                let result = fix.committed.clone() + &compose(&k[fix.replay_from..]).text;
                if result != *w && wrong_result.len() < 10 {
                    wrong_result.push(format!("{w}->{result}"));
                }
            }
        }
        let en_bad: Vec<&&str> = en.iter().filter(|w| fires(&keys(w), &t).is_some()).collect();
        let id_bad: Vec<&&str> = ids.iter().filter(|w| fires(&keys(w), &t).is_some()).collect();
        println!(
            "live fam {f} stem>={stem}: fires on Korean {} (wrong result {:?}) | English hit {} e.g. {:?} | identifiers hit {} e.g. {:?}",
            pct(live, ko.len()),
            wrong_result,
            pct(en_bad.len(), en.len()),
            &en_bad[..en_bad.len().min(12)],
            pct(id_bad.len(), ids.len()),
            &id_bad[..id_bad.len().min(8)]
        );
    }
}
