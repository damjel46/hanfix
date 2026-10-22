//! Precomputes the word tables as sorted text so the engine never builds a map at run time.
//!
//! hanfix-ime loads the engine into every app that takes text input; building hash maps there cost
//! ~42 ms and ~15 MB private memory per process. Sorted `word\tcount` lines embedded with
//! `include_str!` live in the read-only image (shared between processes) and are binary-searched
//! in place (src/lexicon.rs).

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;

fn counts(path: &str) -> Vec<(String, u64)> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter_map(|l| {
            let (w, c) = l.split_once(' ')?;
            Some((w.to_string(), c.trim().parse().ok()?))
        })
        .collect()
}

fn write_table(path: &Path, rows: impl IntoIterator<Item = (String, u64)>) {
    let mut text = String::new();
    for (k, v) in rows {
        text.push_str(&k);
        text.push('\t');
        text.push_str(&v.to_string());
        text.push('\n');
    }
    fs::write(path, text).unwrap();
}

fn main() {
    for f in ["data/en.txt", "data/ko.txt", "data/en_extra.txt", "data/en_dict.txt"] {
        println!("cargo:rerun-if-changed={f}");
    }
    let out = std::env::var("OUT_DIR").unwrap();
    let out = Path::new(&out);

    // English: frequency list + spelling dictionary (count 0 = "known, no frequency").
    let mut en: BTreeMap<String, u64> = counts("data/en.txt").into_iter().collect();
    for w in fs::read_to_string("data/en_extra.txt").unwrap().lines().filter(|l| !l.is_empty()) {
        en.entry(w.to_string()).or_insert(0);
    }
    write_table(&out.join("en_sorted.txt"), en);

    // Spelling dictionary on its own: "a real English word" as opposed to subtitle noise and names.
    let dict: BTreeMap<String, u64> = fs::read_to_string("data/en_dict.txt")
        .unwrap()
        .lines()
        .filter(|l| !l.is_empty())
        .map(|w| (w.to_string(), 1))
        .collect();
    write_table(&out.join("en_dict.txt"), dict);

    // Korean words and per-syllable corpus counts.
    let ko = counts("data/ko.txt");
    let mut syllables: HashMap<char, u64> = HashMap::new();
    for (w, c) in &ko {
        for ch in w.chars() {
            *syllables.entry(ch).or_default() += c;
        }
    }
    write_table(&out.join("ko_sorted.txt"), ko.into_iter().collect::<BTreeMap<_, _>>());
    write_table(&out.join("syllables.txt"), syllables.into_iter().map(|(k, v)| (k.to_string(), v)).collect::<BTreeMap<_, _>>());
}
