//! What loading the engine costs a process (the IME DLL loads into every app), and live-only vs
//! live+Space recall on the test half.
use hanfix::classifier::{judge, live_forward, Mode, Thresholds, Verdict};
use hanfix::dubeolsik::to_keys;
use hanfix::lexicon::Lexicon;
use std::time::Instant;

fn keys(s: &str) -> Vec<(char, bool)> {
    s.chars().map(|c| (c.to_ascii_lowercase(), c.is_ascii_uppercase())).collect()
}

fn main() {
    let t0 = Instant::now();
    let _ = Lexicon::get();
    let load = t0.elapsed();
    if std::env::var("HOLD").is_ok() { std::thread::sleep(std::time::Duration::from_secs(2)); }
    let t1 = Instant::now();
    let k = keys("dkssudgktpdy");
    for _ in 0..1000 {
        std::hint::black_box(judge(&k, Mode::English));
    }
    let per_judge = t1.elapsed() / 1000;
    println!("lexicon load: {load:?}; judge() per word: {per_judge:?}");

    let ko: Vec<String> = include_str!("../tests/data/ko_heldout.txt")
        .lines()
        .filter(|l| !l.is_empty())
        .skip(1)
        .step_by(2)
        .filter_map(to_keys)
        .collect();
    let t = Thresholds::default();
    let live = ko.iter().filter(|w| { let k = keys(w); (1..=k.len()).any(|n| live_forward(&k[..n], &t).is_some()) }).count();
    let any = ko
        .iter()
        .filter(|w| {
            let k = keys(w);
            (1..=k.len()).any(|n| live_forward(&k[..n], &t).is_some()) || judge(&k, Mode::English) != Verdict::Keep
        })
        .count();
    println!("test half: live only {live}/{} ({:.2}%), live + word end {any}/{} ({:.2}%)", ko.len(), 100.0 * live as f64 / ko.len() as f64, ko.len(), 100.0 * any as f64 / ko.len() as f64);
}


