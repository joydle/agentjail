//! Randomized property tests for token parsing and header stripping.
//!
//! These run on stable Rust in CI. A dedicated `cargo-fuzz` target can be
//! layered on later for nightly-only coverage — the parser surface is
//! the same.

use agentjail_phantom::PhantomToken;
use rand::{Rng, RngCore, SeedableRng};

/// Fixed seed so failures reproduce.
const SEED: u64 = 0x_A5A5_5A5A_A5A5_5A5A;
const ITERATIONS: usize = 20_000;

#[test]
fn parse_never_panics_on_arbitrary_input() {
    let mut rng = rand::rngs::StdRng::seed_from_u64(SEED);
    for _ in 0..ITERATIONS {
        let len = rng.gen_range(0..256usize);
        let mut buf = vec![0u8; len];
        rng.fill_bytes(&mut buf);
        // Best-effort UTF-8 conversion; invalid inputs are acceptable.
        let s = String::from_utf8_lossy(&buf);
        let _ = PhantomToken::parse(&s);
    }
}

#[test]
fn roundtrip_holds_for_all_random_tokens() {
    let mut rng = rand::rngs::StdRng::seed_from_u64(SEED);
    for _ in 0..ITERATIONS {
        let t = PhantomToken::generate();
        let s = t.to_string();
        let p = PhantomToken::parse(&s).expect("valid tokens must parse");
        assert!(t.ct_eq(&p));
        // Flipping any single bit must not forge a match.
        let mut mutated = s.clone().into_bytes();
        let bit = rng.gen_range(4..mutated.len()); // skip "phm_" prefix
        mutated[bit] ^= 0x01;
        if let Ok(m) = std::str::from_utf8(&mutated)
            && let Some(q) = PhantomToken::parse(m)
        {
            assert!(!t.ct_eq(&q), "1-bit flip must not still match");
        }
    }
}

#[test]
fn truncated_tokens_never_parse() {
    let t = PhantomToken::generate();
    let full = t.to_string();
    for cut in 0..full.len() {
        let short = &full[..cut];
        assert!(
            PhantomToken::parse(short).is_none(),
            "truncated-to-{cut} parsed as valid: {short:?}"
        );
    }
}

#[test]
fn extended_tokens_never_parse() {
    let t = PhantomToken::generate();
    let base = t.to_string();
    for suffix_len in 1..64 {
        let extended = format!("{base}{}", "a".repeat(suffix_len));
        assert!(
            PhantomToken::parse(&extended).is_none(),
            "over-long token parsed: {extended:?}"
        );
    }
}

#[test]
fn non_hex_chars_rejected_everywhere() {
    let t = PhantomToken::generate();
    let full = t.to_string();
    let full_bytes = full.as_bytes();
    for pos in 4..full_bytes.len() {
        let mut mutated = full_bytes.to_vec();
        mutated[pos] = b'Z';
        let s = String::from_utf8(mutated).unwrap();
        assert!(
            PhantomToken::parse(&s).is_none(),
            "non-hex at {pos} still parsed: {s:?}"
        );
    }
}
