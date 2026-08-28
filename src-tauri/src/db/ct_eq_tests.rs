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

// --- Length-truncation regression tests (review of #1240) ---
//
// The original `ct_eq` cast `(a.len() ^ b.len()) as u8` truncated the length
// difference to 8 bits. A length difference whose XOR is a multiple of 256
// (e.g. 0 vs 256, 32 vs 288) collapsed to 0 as `u8`, allowing a malicious
// longer input whose extra bytes are all zero to falsely match a shorter
// stored secret. On the root-token path this is a direct authentication
// bypass: an attacker who has learned the 32-char root token can present
// `root_token || [0u8; 256]` and be authenticated.
//
// These tests are pinned so the bug cannot silently regress.

#[test]
fn empty_vs_256_zeros_is_false() {
    // The direct reproducer: the (0, 256) length difference has XOR 256, which
    // truncates to 0 as u8. Combined with the all-zero 256-byte payload, the
    // original implementation returned `true` for this pair.
    let zeros_256 = [0u8; 256];
    assert!(!ct_eq(b"", &zeros_256));
    assert!(!ct_eq(&zeros_256, b""));
}

#[test]
fn root_token_plus_256_zero_bytes_does_not_match_root_token() {
    // The realistic attack against `validate_root_token_inner`: present the
    // 32-byte root token followed by 256 zero bytes. Lengths 32 and 288 have
    // XOR 256, which truncates to 0 as u8. With the original implementation
    // this falsely returned `true` — granting Admin role.
    let root_token = b"0123456789abcdef0123456789abcdef"; // 32 hex chars
    let mut extended = [0u8; 32 + 256];
    extended[..32].copy_from_slice(root_token);
    assert!(!ct_eq(root_token, &extended));
    assert!(!ct_eq(&extended, root_token));
}

#[test]
fn length_difference_xor_that_truncates_to_zero_u8_is_false() {
    // Generalised: any pair of lengths (a, b) where (a ^ b) is a positive
    // multiple of 256 must produce a non-zero length-XOR at the diff width
    // the implementation uses. We sweep a handful of those pairs and assert
    // the input is rejected regardless of byte content.
    let pairs: &[(usize, usize)] = &[
        (0, 256),
        (1, 257),
        (32, 288),
        (64, 320),
        (128, 384),
        (255, 511),
    ];
    for &(la, lb) in pairs {
        let a = vec![0u8; la];
        let b = vec![0u8; lb];
        assert!(
            !ct_eq(&a, &b),
            "ct_eq falsely matched len {} vs len {} (length-XOR 0x{:x} truncates to zero)",
            la,
            lb,
            la ^ lb
        );
        assert!(
            !ct_eq(&b, &a),
            "ct_eq falsely matched len {} vs len {} (swapped)",
            lb,
            la
        );
    }
}
