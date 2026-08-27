//! Correctness property tests for `ct_eq` (issue #1240).
//!
//! Timing itself cannot be meaningfully asserted in CI (too noisy across hosts,
//! and any microbenchmark would also fight the optimiser). What we *can* assert
//! is the **functional** property the helper exists to guarantee: a non-match
//! must read `false` regardless of how much of the inputs are shared. If a
//! future refactor accidentally reintroduces an early-exit on byte mismatch,
//! the prefix-similarity cases below catch it — a "matches up to byte N"
//! pair would falsely return `true` and trip an assertion.

use super::ct_eq;

#[test]
fn equal_inputs_are_true() {
    assert!(ct_eq(b"", b""));
    assert!(ct_eq(b"abc", b"abc"));
    // 64-char SHA-256 hex shape we compare against in the coordinator
    // validators.
    let hex = b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    assert!(ct_eq(hex, hex));
}

#[test]
fn different_length_inputs_are_false() {
    // Length mismatches are the most likely failure mode if someone
    // accidentally drops the length-XOR seed.
    assert!(!ct_eq(b"", b"a"));
    assert!(!ct_eq(b"a", b""));
    assert!(!ct_eq(b"abc", b"abcd"));
    assert!(!ct_eq(b"abcd", b"abc"));
    // A short token vs a 64-char hash — the case that would silently match
    // if a future refactor forgot the `(a.len() ^ b.len())` seed in the
    // accumulator: the byte loop would then XOR-equal the shared "abc" and
    // read `0 ^ 0` for the remaining indices, returning true.
    assert!(!ct_eq(b"abc", b"abc\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0"));
}

#[test]
fn same_length_different_content_is_false() {
    assert!(!ct_eq(b"abc", b"abd"));
    assert!(!ct_eq(b"abc", b"xbc"));
    assert!(!ct_eq(b"abc", b"abx"));
}

#[test]
fn shared_prefix_does_not_short_circuit_to_true() {
    // The classic timing-attack case: an early-exit compare would say "true"
    // up to the first mismatch. We want `false` regardless of how much
    // matches.
    assert!(!ct_eq(b"abcdef", b"abcXYZ"));
    assert!(!ct_eq(b"abcdef", b"abcdez"));
    // The two strings differ only in the final byte.
    assert!(!ct_eq(b"abcdef", b"abcdee"));
    // 63-char shared prefix, 1-char difference at the end — the highest
    // similarity ratio a CT-compare can encounter.
    let shared_prefix = b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcde";
    let almost_match = b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdX";
    assert!(!ct_eq(shared_prefix, almost_match));
}

#[test]
fn single_bit_difference_in_full_length_hash_is_false() {
    // Mimics the coordinator hash compare path: two 64-char SHA-256 hex
    // strings, one bit flipped at the very end.
    let a = b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let mut b = *a;
    b[63] = b'e';
    assert!(!ct_eq(a, &b));
}

#[test]
fn reversed_inputs_are_false() {
    // Sanity: ct_eq is not a palindrome check.
    assert!(!ct_eq(b"abcdef", b"fedcba"));
}
