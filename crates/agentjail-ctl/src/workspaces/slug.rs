//! Memorable auto-labels for workspaces.
//!
//! Format: `{adjective}-{noun}-{4hex}`, e.g. `quiet-falcon-3fa2`. The
//! 4-hex suffix keeps collisions unlikely even when the adjective/noun
//! pair repeats, so auto-generated labels stay unique-ish without a
//! database uniqueness check.

use rand::RngCore;
use rand::seq::SliceRandom;

const ADJECTIVES: &[&str] = &[
    "amber", "bold", "brave", "bright", "brisk", "calm", "clever", "cozy",
    "crisp", "deft", "eager", "fair", "feisty", "fleet", "glossy", "hazy",
    "jolly", "keen", "lively", "lucid", "merry", "mild", "misty", "nimble",
    "noble", "plucky", "quiet", "rapid", "rustic", "silent", "sleek", "snug",
    "stoic", "sturdy", "sunny", "swift", "tidy", "vivid", "warm", "wise",
];

const NOUNS: &[&str] = &[
    "arbor", "badger", "basin", "beacon", "birch", "bison", "canyon", "cedar",
    "comet", "cove", "delta", "ember", "falcon", "fern", "forge", "fox",
    "harbor", "hawk", "heron", "kite", "lark", "lynx", "marsh", "meadow",
    "moss", "mountain", "orchid", "otter", "owl", "pine", "plover", "quill",
    "raven", "ridge", "river", "sparrow", "swan", "thicket", "valley", "willow",
];

/// Generate a fresh auto-label, e.g. `quiet-falcon-3fa2`.
#[must_use]
pub fn generate() -> String {
    let mut rng = rand::thread_rng();
    let adj = ADJECTIVES.choose(&mut rng).copied().unwrap_or("swift");
    let noun = NOUNS.choose(&mut rng).copied().unwrap_or("falcon");
    let suffix: u16 = (rng.next_u32() & 0xFFFF) as u16;
    format!("{adj}-{noun}-{suffix:04x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shape_is_three_hyphen_parts() {
        for _ in 0..100 {
            let s = generate();
            let parts: Vec<&str> = s.split('-').collect();
            assert_eq!(parts.len(), 3, "got {s}");
            assert!(!parts[0].is_empty());
            assert!(!parts[1].is_empty());
            assert_eq!(parts[2].len(), 4);
            assert!(parts[2].chars().all(|c| c.is_ascii_hexdigit()));
        }
    }
}
