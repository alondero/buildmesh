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

#[test]
fn devin_session_ids_accept_live_petname_shapes() {
    for id in ["orchid-ragamuffin", "sneaky-biology", "rumbling-path"] {
        assert_eq!(
            parse_session_id_for_provider("devin", id),
            Some(id.into()),
            "live petname id must round-trip"
        );
        assert_eq!(
            parse_session_id_for_provider("anthropic", id),
            None,
            "other providers must not adopt the petname shape"
        );
    }
    for invalid in [
        "",
        "ab",
        "-orchid-ragamuffin",
        "--resume",
        "orchid ragamuffin",
        "orchid_ragamuffin",
        "Orchid-Ragamuffin",
        "orchid-$(whoami)",
        "orchid/ragamuffin",
        "orchid.ragamuffin",
    ] {
        assert_eq!(
            parse_session_id_for_provider("devin", invalid),
            None,
            "{invalid:?} must be rejected"
        );
    }
    let too_long = format!("o{}", "-r".repeat(64));
    assert_eq!(parse_session_id_for_provider("devin", &too_long), None);
}
