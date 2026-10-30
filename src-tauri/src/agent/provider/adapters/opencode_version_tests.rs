use super::*;

#[test]
fn versions_are_strict_and_shared_binary_majors_stay_distinct() {
    assert_eq!(major("opencode v2.2.3\r\n"), Some(2));
    assert_eq!(major("1.18.3\n"), Some(1));
    assert_eq!(major("v3.0.0"), Some(3));
    for malformed in ["", "2", "opencode unknown", "error 2.2.3", "2.x.3"] {
        assert_eq!(major(malformed), None);
    }
}
