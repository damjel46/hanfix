//! Prints "word<TAB>keys" for the first N Korean held-out words (input for engine comparisons).
fn main() {
    let n: usize = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(2000);
    for w in include_str!("../tests/data/ko_heldout.txt").lines().filter(|l| !l.is_empty()).take(n) {
        if let Some(k) = hanfix::dubeolsik::to_keys(w) {
            println!("{w}\t{k}");
        }
    }
}
