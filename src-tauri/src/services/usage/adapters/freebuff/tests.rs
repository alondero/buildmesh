use super::*;

#[test]
fn usage_link_opens_freebuff_settings() {
    assert_eq!(
        FreebuffAdapter.usage_page().unwrap().url,
        "https://freebuff.com/?settings=usage"
    );
}
