//! Saved cards through the controller, on a disposable vault with the
//! memory key store: the Settings list, its editor and fills. Only the card
//! networks' published test numbers and invented names are used.

use super::*;
use crate::passwords::tests::Harness;
use crossbow_core::vault::{CardHint, CardNetwork};

const A: &str = "fixture-profile-a";
const B: &str = "fixture-profile-b";
const VISA: &str = "4242 4242 4242 4242";
const VISA_DIGITS: &str = "4242424242424242";
const MASTERCARD: &str = "5555-5555-5555-4444";

fn unlocked() -> Harness {
    let mut h = Harness::new();
    h.passwords.set_profiles(A, [(A.to_owned(), "Personal".to_owned()), (B.to_owned(), "Work".to_owned())]);
    h.passwords.status().unwrap();
    h.passwords.unlock();
    assert!(h.passwords.unlocked());
    h
}

/// Type into the open editor, as its fields would.
fn type_into(h: &mut Harness, name: &str, number: &str, month: &str, year: &str, nickname: &str) {
    let editor = h.passwords.page.card_editor.as_mut().expect("the editor is open");
    for (field, text) in [(&mut editor.name, name), (&mut editor.number, number), (&mut editor.month, month), (&mut editor.year, year), (&mut editor.nickname, nickname)] {
        field.clear();
        field.push_str(text);
    }
}

fn add(h: &mut Harness, profile: &str, number: &str, nickname: &str) -> String {
    let card = Card {
        name: Zeroizing::new("Fixture Cardholder".into()), number: Zeroizing::new(number.into()), exp_month: 7, exp_year: 2034, nickname: Zeroizing::new(nickname.into()),
    };
    h.passwords.vault_mut().unwrap().add_card(profile, &card).unwrap()
}

fn listed(h: &Harness) -> Vec<String> {
    h.passwords.listed_cards().iter().map(|saved| card_title(saved)).collect()
}

#[test]
fn cards_are_added_edited_and_deleted_in_the_active_profile_and_survive_a_reopen() {
    let mut h = unlocked();
    h.passwords.show_section(Section::Cards);
    h.passwords.start_add_card();
    let editor = h.passwords.page.card_editor.as_ref().unwrap();
    assert_eq!((editor.id.as_deref(), editor.profile.as_str(), editor.focus, editor.saved.as_deref()), (None, A, true, None), "Add saves into the active profile");
    type_into(&mut h, "Fixture Cardholder", VISA, "7", "2034", "Fixture Travel");
    h.passwords.save_card_editor();
    assert_eq!(h.passwords.page.notice.as_deref(), Some(CARD_ADDED));
    assert!(h.passwords.page.card_editor.is_none());
    assert_eq!(listed(&h), ["Visa ••••4242"]);
    let id = h.passwords.listed_cards()[0].id.clone();
    let saved = h.passwords.vault().unwrap().saved_card(&id).unwrap();
    assert_eq!((saved.expiry(), saved.nickname.as_str(), saved.name.as_str()), ("07/34".to_owned(), "Fixture Travel", "Fixture Cardholder"));
    h.passwords.set_profiles(B, [(A.to_owned(), "Personal".to_owned()), (B.to_owned(), "Work".to_owned())]);
    assert!(listed(&h).is_empty(), "another profile's card isn't listed");
    h.passwords.set_profiles(A, [(A.to_owned(), "Personal".to_owned()), (B.to_owned(), "Work".to_owned())]);

    // Edit opens with the number blank, named only by its network and last four.
    h.passwords.start_edit_card(&id);
    let editor = h.passwords.page.card_editor.as_ref().unwrap();
    assert_eq!((editor.number.as_str(), editor.saved.as_deref(), editor.month.as_str(), editor.year.as_str()), ("", Some("Visa ••••4242"), "07", "2034"));
    assert_eq!((editor.name.as_str(), editor.nickname.as_str()), ("Fixture Cardholder", "Fixture Travel"));
    type_into(&mut h, "Fixture Cardholder", "", "8", "2035", "Fixture Shopping");
    h.passwords.save_card_editor();
    assert_eq!(h.passwords.page.notice.as_deref(), Some(CARD_SAVED));
    let saved = h.passwords.vault().unwrap().saved_card(&id).unwrap();
    assert_eq!((saved.expiry(), saved.nickname.as_str(), saved.last4.as_str()), ("08/35".to_owned(), "Fixture Shopping", "4242"), "a blank number keeps the saved one");
    assert_eq!(h.passwords.card_to_fill(&id, A).unwrap()[1].as_str(), VISA_DIGITS);

    // A new number replaces the old one.
    h.passwords.start_edit_card(&id);
    let editor = h.passwords.page.card_editor.as_mut().unwrap();
    editor.number.push_str(MASTERCARD);
    h.passwords.save_card_editor();
    assert_eq!(listed(&h), ["Mastercard ••••4444"]);
    assert_eq!(h.passwords.card_to_fill(&id, A).unwrap()[1].as_str(), "5555555555554444");

    h.reopen();
    h.passwords.set_profiles(A, [(A.to_owned(), "Personal".to_owned()), (B.to_owned(), "Work".to_owned())]);
    h.passwords.status().unwrap();
    assert!(h.passwords.listed_cards().is_empty(), "locked after a reopen");
    h.passwords.unlock();
    assert_eq!(listed(&h), ["Mastercard ••••4444"], "kept after a reopen");

    h.passwords.request_delete_card(&id);
    assert_eq!(h.passwords.page.confirm_delete_card.as_deref(), Some(id.as_str()));
    h.passwords.confirm_delete_card();
    assert_eq!(h.passwords.page.notice.as_deref(), Some(CARD_DELETED));
    assert!(h.passwords.vault().unwrap().cards().is_empty());
    h.passwords.page.confirm_delete_card = Some(id);
    h.passwords.confirm_delete_card();
    assert_eq!(h.passwords.page.notice.as_deref(), Some(CARD_ALREADY_DELETED));
}

#[test]
fn hints_never_block_a_save_and_only_what_the_vault_cant_hold_keeps_the_draft() {
    let mut h = unlocked();
    h.passwords.start_add_card();
    type_into(&mut h, "", "4242 4242 4242 4241", "1", "2020", "");
    let hints: Vec<CardHint> = h.passwords.page.card_editor.as_ref().unwrap().hints((2026, 10)).into_iter().map(|(_, hint)| hint).collect();
    assert_eq!(hints, [CardHint::FailsCheck, CardHint::Expired]);
    h.passwords.save_card_editor();
    assert_eq!(h.passwords.page.notice.as_deref(), Some(CARD_ADDED), "saved as typed");

    h.passwords.start_add_card();
    type_into(&mut h, "", "6767 6767 6767 6767 67", "12", "31", "");
    let hints: Vec<CardHint> = h.passwords.page.card_editor.as_ref().unwrap().hints((2026, 10)).into_iter().map(|(_, hint)| hint).collect();
    assert!(hints.contains(&CardHint::UnknownNetwork), "{hints:?}");
    h.passwords.save_card_editor();
    assert_eq!(h.passwords.page.notice.as_deref(), Some(CARD_ADDED));
    let unknown = h.passwords.listed_cards().into_iter().find(|saved| saved.network == CardNetwork::Unknown).expect("the unknown card");
    assert_eq!(unknown.expiry(), "12/31", "a two-digit year is this century's");

    for (number, month, year, error) in [
        ("4242-abcd-4242", "7", "2034", "A card number has only digits."),
        ("", "7", "2034", "Enter the card number."),
        (VISA, "13", "2034", "Enter the month the card expires, 1 to 12."),
        (VISA, "7", "", "Enter the year the card expires, such as 2031."),
        (VISA, "July", "2034", "Enter the month the card expires, 1 to 12."),
    ] {
        h.passwords.start_add_card();
        type_into(&mut h, "Fixture Cardholder", number, month, year, "Draft");
        h.passwords.save_card_editor();
        let editor = h.passwords.page.card_editor.as_ref().expect("the draft is kept for a retry");
        assert_eq!((editor.error.as_deref(), editor.nickname.as_str(), editor.number.as_str()), (Some(error), "Draft", number), "{number} {month} {year}");
    }
    assert_eq!(h.passwords.vault().unwrap().cards().len(), 2, "nothing the vault can't hold was saved");
}

#[test]
fn locking_or_leaving_the_page_drops_the_card_draft_and_every_opened_card() {
    let mut h = unlocked();
    add(&mut h, A, VISA, "Fixture Travel");
    h.passwords.start_add_card();
    type_into(&mut h, "Fixture Cardholder", MASTERCARD, "12", "2031", "");
    h.passwords.tick_at(None, false);
    assert!(h.passwords.page.card_editor.is_none(), "leaving the page drops the draft");
    let id = h.passwords.listed_cards()[0].id.clone();
    h.passwords.start_edit_card(&id);
    h.passwords.request_delete_card(&id);
    h.passwords.lock(None);
    assert!(h.passwords.page.card_editor.is_none() && h.passwords.page.confirm_delete_card.is_none());
    assert!(h.passwords.vault().unwrap().cards().is_empty(), "locking drops every opened card");
    assert!(h.passwords.listed_cards().is_empty() && h.passwords.cards_for(A).is_empty());
    assert_eq!(h.passwords.card_to_fill(&id, A).map(|_| ()), Err(CARD_GONE), "nothing fills while locked");
}

#[test]
fn a_page_gets_only_a_card_of_its_own_profile_and_its_number_only_for_the_fill() {
    let mut h = unlocked();
    let visa = add(&mut h, A, VISA, "Fixture Travel");
    add(&mut h, A, MASTERCARD, "");
    let other = add(&mut h, B, VISA, "Work card");
    let menu: Vec<(String, String)> = h.passwords.cards_for(A).into_iter().map(|(_, label, expiry)| (label, expiry)).collect();
    assert_eq!(menu, [("Mastercard ••••4444".to_owned(), "07/34".to_owned()), ("Visa ••••4242".to_owned(), "07/34".to_owned())]);
    assert_eq!(h.passwords.cards_for(B).len(), 1);
    assert!(h.passwords.vault().unwrap().saved_card(&visa).unwrap().used_ms.is_none());
    let parts = h.passwords.card_to_fill(&visa, A).unwrap();
    assert_eq!(parts.each_ref().map(|part| part.as_str()), ["Fixture Cardholder", VISA_DIGITS, "07", "2034"]);
    assert!(h.passwords.vault().unwrap().saved_card(&visa).unwrap().used_ms.is_some(), "the fill is recorded");
    assert_eq!(h.passwords.card_to_fill(&other, A).map(|_| ()), Err(CARD_GONE), "another profile's card");
    assert_eq!(h.passwords.card_to_fill("no-such-card", A).map(|_| ()), Err(CARD_GONE));
}

#[test]
fn the_key_button_offers_the_profiles_cards_or_unlock_while_locked() {
    let mut h = unlocked();
    assert_eq!(h.passwords.card_key_state(A), None, "no card saved");
    add(&mut h, A, VISA, "");
    add(&mut h, A, MASTERCARD, "");
    assert_eq!(h.passwords.card_key_state(A), Some(KeyState::Cards(2)));
    assert_eq!(h.passwords.card_key_state(B), None);
    h.passwords.lock(None);
    assert_eq!(h.passwords.card_key_state(B), Some(KeyState::Locked), "locked, any card may be this profile's");
}

#[test]
fn reset_counts_cards_and_recover_reports_them_on_their_own() {
    let mut h = unlocked();
    add(&mut h, A, VISA, "");
    add(&mut h, B, MASTERCARD, "");
    assert_eq!(h.passwords.reset_scope(), ResetScope::Entries { passwords: 0, addresses: 0, cards: 2 });
    h.passwords.lock(None);
    assert_eq!(h.passwords.reset_scope(), ResetScope::Entries { passwords: 0, addresses: 0, cards: 2 }, "counted while locked too");

    let counted = |recovered, found, complete| crossbow_core::vault::Counted { recovered, found, complete };
    let kept = "kept as vault.damaged.sqlite in Crossbow's data folder, readable only by your user account.";
    let recovery = |cards| Recovery { recovered: 3, found: 4, complete: true, addresses: counted(0, 0, true), cards, never_save: counted(0, 0, true), moved: "vault.damaged.sqlite".into(), also_kept: vec![] };
    assert_eq!(recovered(&recovery(counted(0, 0, true))), format!("Recovered 3 of 4 passwords. 1 couldn't be read; the damaged file is {kept}"), "without cards it reads as before");
    assert_eq!(recovered(&recovery(counted(2, 3, true))), format!("Recovered 3 of 4 passwords and 2 of 3 cards. 1 password and 1 card couldn't be read; the damaged file is {kept}"));
    assert_eq!(recovered(&recovery(counted(1, 1, false))), format!("Recovered 3 of 4 passwords and 1 of 1 card. The cards couldn't be read in full. 1 couldn't be read; the damaged file is {kept}"));
}

#[test]
fn no_card_number_name_expiry_or_nickname_reaches_the_log_or_debug_output() {
    let capture = crate::passwords::page_logins::tests::captured_logs();
    let mut h = unlocked();
    let id = add(&mut h, A, VISA, "Fixture-Nickname-Travel");
    h.passwords.start_add_card();
    type_into(&mut h, "Fixture-Holder-Draft", MASTERCARD, "12", "2031", "Fixture-Nickname-Draft");
    h.passwords.save_card_editor();
    h.passwords.start_edit_card(&id);
    let _ = h.passwords.card_to_fill(&id, A);
    let _ = h.passwords.card_to_fill(&id, B);
    let printed = format!("{:?} {:?} {:#?}", h.passwords.page.card_editor, h.passwords.listed_cards(), h.passwords.vault().unwrap().cards());
    h.passwords.confirm_delete_card();
    h.passwords.lock(None);
    let lines = capture.0.lock().unwrap().clone();
    for secret in [VISA_DIGITS, VISA, "5555555555554444", MASTERCARD, "Fixture Cardholder", "Fixture-Holder-Draft", "Fixture-Nickname-Travel", "Fixture-Nickname-Draft", "2034", "2031", "07/34"] {
        assert!(lines.iter().all(|line| !line.contains(secret)), "{secret:?} was logged: {lines:?}");
        assert!(!printed.contains(secret), "{secret:?} is in debug output: {printed}");
    }
    assert!(printed.contains("Visa") && printed.contains("4242"), "Debug names a card by its network and last four: {printed}");
}
