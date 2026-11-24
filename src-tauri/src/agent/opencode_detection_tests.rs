use super::*;

#[test]
fn v2_alias_is_discovered_even_when_an_older_opencode_precedes_it_on_path() {
    let v1 = PathBuf::from("/legacy/opencode");
    let v2 = PathBuf::from("/npm/opencode2");
    let profiles = detect_profiles(
        &[PathBuf::from("/legacy"), PathBuf::from("/npm")],
        &[""],
        None,
        &|path| path == v1 || path == v2,
    );
    let confirmed = confirm_versions(profiles, &|path| (path == v2).then_some(2));
    let executables: Vec<_> = confirmed
        .iter()
        .map(|p| (p.harness.as_str(), p.executable.as_deref()))
        .collect();
    assert_eq!(executables, [("opencode2", Some(v2.as_path()))]);
}

#[test]
fn v2_requires_a_confirmed_v2_major() {
    let profiles = detect_profiles(&[PathBuf::from("/npm")], &[""], None, &|path| {
        path == Path::new("/npm/opencode2")
    });
    assert_eq!(profiles.len(), 1, "the V2 alias is a discovery candidate");
    for major in [None, Some(1), Some(3)] {
        assert!(confirm_versions(profiles.clone(), &|_| major).is_empty());
    }
    assert_eq!(
        confirm_versions(profiles, &|_| Some(2))[0].harness,
        "opencode2"
    );
}

#[test]
fn unknown_version_withholds_v2() {
    let profiles = detect_profiles(&[PathBuf::from("/bin")], &[""], None, &|p| {
        p == Path::new("/bin/opencode")
    });
    assert!(confirm_versions(profiles, &|_| None).is_empty());
}

#[test]
fn known_majors_accept_only_v2_with_one_probe_per_path() {
    let profiles = detect_profiles(&[PathBuf::from("/bin")], &[""], None, &|p| {
        p == Path::new("/bin/opencode")
    });
    assert_eq!(profiles.len(), 1);
    let calls = std::cell::Cell::new(0);
    for major in [1, 2, 3] {
        calls.set(0);
        let confirmed = confirm_versions(profiles.clone(), &|_| {
            calls.set(calls.get() + 1);
            Some(major)
        });
        assert_eq!(calls.get(), 1);
        if major == 2 {
            assert_eq!(confirmed.len(), 1);
            assert_eq!(confirmed[0].harness, "opencode2");
        } else {
            assert!(confirmed.is_empty());
        }
    }
}

#[test]
fn cross_runtime_probes_cannot_claim_an_unverified_major() {
    for tool in DETECTABLE.iter().filter(|t| t.major_version.is_some()) {
        assert!(!wsl_probed(tool));
    }
}

#[test]
fn a_pin_edited_while_version_probe_is_pending_wins_over_the_stale_result() {
    let mut profile = detect_profiles(&[PathBuf::from("/bin")], &[""], None, &|p| {
        p == Path::new("/bin/opencode")
    })
    .into_iter()
    .find(|p| p.harness == "opencode2")
    .unwrap();
    profile.executable = Some(PathBuf::from("/old/opencode"));
    let baseline = vec![profile.clone()];
    let current = std::cell::RefCell::new(baseline.clone());
    let confirmed = confirm_versions(vec![profile], &|_| {
        current.borrow_mut()[0].executable = Some(PathBuf::from("/new/opencode"));
        Some(2)
    });
    assert_eq!(confirmed.len(), 1);
    let admitted = merge_confirmed_versions(&baseline, &mut current.borrow_mut(), confirmed);
    assert!(admitted.is_empty());
    assert_eq!(
        current.borrow()[0].executable.as_deref(),
        Some(Path::new("/new/opencode"))
    );
    let mut unchanged = baseline.clone();
    assert_eq!(
        merge_confirmed_versions(&baseline, &mut unchanged, baseline.clone()),
        baseline
    );
}

#[test]
fn deferred_publication_updates_detection_before_notifying_open_menus() {
    struct Restore(DetectionSnapshot);
    impl Drop for Restore {
        fn drop(&mut self) {
            restore_detection_state(&self.0);
        }
    }
    let _restore = Restore(detection_snapshot());
    observe_installations(HashSet::new());
    let profile = detect_profiles(&[PathBuf::from("/bin")], &[""], None, &|p| {
        p == Path::new("/bin/opencode")
    })
    .into_iter()
    .find(|p| p.harness == "opencode2")
    .unwrap();
    let notified = std::cell::Cell::new(0);
    publish_versioned_profiles(vec![profile.clone()], || {
        assert!(detection_snapshot().installed.contains("opencode2"));
        notified.set(notified.get() + 1);
    });
    assert_eq!(notified.get(), 1);
    publish_versioned_profiles(vec![profile], || notified.set(notified.get() + 1));
    assert_eq!(
        notified.get(),
        1,
        "identical publications do not invalidate menus repeatedly"
    );
}
