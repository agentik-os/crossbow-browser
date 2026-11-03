use super::*;
use crate::update::fixtures::FakeCode;

#[test]
fn the_requirement_names_developer_id_this_bundle_and_the_team() {
    let text = requirement("ABCDE12345").unwrap();
    assert!(text.starts_with("identifier \"app.crossbow.Crossbow\" and anchor apple generic"));
    assert!(text.contains("certificate leaf[field.1.2.840.113635.100.6.1.13] exists"), "a Developer ID Application certificate");
    assert!(text.ends_with("certificate leaf[subject.OU] = \"ABCDE12345\""));
    for bad in ["", "abcde12345", "ABCDE1234", "ABCDE123456", "ABCDE1234\"", "ABCDE 1234"] {
        assert_eq!(requirement(bad), None, "{bad:?} can't be spliced into a requirement");
    }
}

#[test]
fn a_new_app_needs_a_valid_developer_id_signature_and_the_same_team_to_install_itself() {
    let app = Path::new("/nowhere/Crossbow.app");
    let signed = |team: &str, install| Ok(Signed { team: team.into(), install });
    assert_eq!(check(&FakeCode { team: Some("ABCDE12345".into()), valid: false, signed_by: "ABCDE12345".into() }, app), Err(CodeError::Invalid));
    assert_eq!(check(&FakeCode { team: Some("ABCDE12345".into()), valid: true, signed_by: "ZYXWV98765".into() }, app), Err(CodeError::Team));
    assert_eq!(check(&FakeCode { team: Some("not a team".into()), valid: true, signed_by: "not a team".into() }, app), Err(CodeError::Team));
    assert_eq!(check(&FakeCode::signed("ABCDE12345"), app), signed("ABCDE12345", Ok(())));
    // An ad-hoc signed running app still checks the new app, against any
    // Developer ID team, and leaves installing it to the user.
    assert_eq!(check(&FakeCode { team: None, valid: true, signed_by: "ZYXWV98765".into() }, app), signed("ZYXWV98765", Err(CodeError::AdHoc)));
    assert_eq!(check(&FakeCode { team: None, valid: false, signed_by: "ZYXWV98765".into() }, app), Err(CodeError::Invalid));
    assert_eq!(check(&FakeCode { team: None, valid: true, signed_by: String::new() }, app), Err(CodeError::DeveloperId), "no Team ID at all");
    assert_eq!(check(&FakeCode { team: None, valid: true, signed_by: "not a team".into() }, app), Err(CodeError::DeveloperId));
    /// Valid code whose signature names a team it does not meet the Developer ID requirement for.
    struct Mismatch;
    impl CodeCheck for Mismatch {
        fn running_team(&self) -> Option<String> { None }
        fn signer(&self, _: &Path) -> Option<String> { Some("ABCDE12345".into()) }
        fn valid(&self, _: &Path) -> bool { true }
        fn meets(&self, _: &Path, _: &str) -> bool { false }
    }
    assert_eq!(check(&Mismatch, app), Err(CodeError::DeveloperId));
}

#[cfg(target_os = "macos")]
#[test]
fn this_ad_hoc_signed_test_process_has_no_team_and_refuses_code_without_a_developer_id() {
    let executable = std::env::current_exe().unwrap();
    assert!(mac::identifier(&executable).is_some(), "its signature is read: ad hoc, with an identifier");
    assert_eq!(System.running_team(), None, "the linker signs test binaries ad hoc");
    assert_eq!(team_of(&executable), None);
    assert_eq!(System.signer(&executable), None);
    assert_eq!(check(&System, &executable), Err(CodeError::DeveloperId), "valid, but ad hoc: no Developer ID");
    assert!(!System.meets(&executable, "ABCDE12345"), "an ad-hoc signature meets no Developer ID requirement");
    let dir = tempfile::tempdir().unwrap();
    let plain = dir.path().join("plain.txt");
    std::fs::write(&plain, "not code").unwrap();
    assert!(!System.valid(&plain), "a file with no signature is not valid code");
    assert_eq!(check(&System, &plain), Err(CodeError::Invalid));
    assert!(!System.valid(&dir.path().join("missing")));
    // A control: Apple's own signed tools are valid code, of no Developer ID team.
    assert!(System.valid(Path::new("/bin/ls")));
    assert!(!System.meets(Path::new("/bin/ls"), "ABCDE12345"));
    assert_eq!(check(&System, Path::new("/bin/ls")), Err(CodeError::DeveloperId));
}
