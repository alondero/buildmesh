use super::*;

#[test]
fn parse_session_id_for_provider_dispatches_devin_to_slug_validator() {
    for slug in ["phantom-jellyfish", "tangy-peony", "orchid-ragamuffin"] {
        assert_eq!(
            parse_session_id_for_provider("devin", slug).as_deref(),
            Some(slug),
        );
    }
    assert_eq!(
        parse_session_id_for_provider("devin", "3F8D1C2A-1CB0-408C-B29C-9F0F68F2982B").as_deref(),
        Some("3f8d1c2a-1cb0-408c-b29c-9f0f68f2982b"),
    );
    for bad in [
        "ses_fc52ccfb9ffek1jl23ZwpRuSP7",
        "mvs_d66c5fa695294e2abe936c242fb43c76",
        "--dangerously-skip-permissions",
        "tangy-peony;rm -rf /",
        "$(whoami)",
    ] {
        assert_eq!(parse_session_id_for_provider("devin", bad), None, "{bad}");
    }
}
