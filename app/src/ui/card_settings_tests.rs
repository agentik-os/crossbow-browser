//! Settings → Passwords → Cards driven by simulated pointer and keyboard
//! input in the real Settings sheet geometry, at 640×420 and 1360×860, on a
//! disposable vault with the memory key store. Only the card networks'
//! published test numbers and invented names are used.

use super::*;
use crate::passwords::{CARD_ADDED, CARD_DELETED, CARD_SAVED, Section, card_field_id};
use crossbow_core::vault::{Card, CardHint, Zeroizing};

const A: &str = "fixture-profile-a";
const B: &str = "fixture-profile-b";
const EMPTY: &str = "No saved cards in this profile yet.";
const VISA: &str = "4242 4242 4242 4242";
const VISA_DIGITS: &str = "4242424242424242";
const MASTERCARD: &str = "5555 5555 5555 4444";

fn profiles(page: &mut Sheet, current: &str) {
    page.h.passwords.set_profiles(current, [(A.to_owned(), "Personal".to_owned()), (B.to_owned(), "Work".to_owned())]);
}

fn sheet(size: Vec2) -> Sheet {
    let mut page = Sheet::new(size);
    profiles(&mut page, A);
    page
}

fn card(number: &str, nickname: &str) -> Card {
    Card { name: Zeroizing::new("Fixture Cardholder".into()), number: Zeroizing::new(number.into()), exp_month: 7, exp_year: 2034, nickname: Zeroizing::new(nickname.into()) }
}

impl Sheet {
    /// No text drawn anywhere, from the top of the page to its bottom, holds
    /// a saved card's full number, in any spacing.
    fn never_shows_a_full_number(&mut self) {
        for _ in 0..10 { self.wheel(1000.0); }
        for _ in 0..30 {
            let shown = self.settle();
            for number in [VISA, VISA_DIGITS, MASTERCARD, "5555555555554444"] {
                assert!(!shown.mentions(number), "{number} is drawn at {:?}", self.size);
            }
            self.wheel(-100.0);
        }
    }
}

#[test]
fn cards_are_added_edited_and_deleted_with_the_pointer_and_kept_after_a_reopen() {
    for size in sizes() {
        let mut page = sheet(size);
        page.click("Unlock");
        // Three segments side by side, inside the sheet at either size.
        let shown = page.visible("Cards");
        let (passwords, addresses, cards) = (shown.find("Passwords", true).unwrap().1, shown.find("Addresses", false).unwrap().1, shown.find("Cards", false).unwrap().1);
        assert!(passwords.right() < addresses.left() && addresses.right() < cards.left() && (cards.center().y - passwords.center().y).abs() < 1.0, "one row at {size:?}");
        page.fits_width();
        page.click("Cards");
        assert_eq!(page.h.passwords.page.section, Section::Cards);
        page.visible(EMPTY);
        assert!(!page.appears("Add Password") && !page.appears("Add Address"), "the other lists are hidden at {size:?}");
        page.fits_width();
        page.click("Add Card");
        assert_eq!(page.focused(), Some(card_field_id(0)), "Add puts focus on the number at {size:?}");
        page.type_text(VISA);
        assert!(page.settle().has(VISA), "a new number shows while it is typed at {size:?}");
        page.focus_field("card-name");
        assert!(!page.settle().mentions(VISA), "and is masked once the field is left at {size:?}");
        page.type_text("Fixture Cardholder");
        page.focus_field("card-month");
        page.type_text("7");
        page.focus_field("card-year");
        page.type_text("2034");
        page.focus_field("card-nickname");
        page.type_text("Fixture Travel");
        page.visible("NEW CARD");
        page.fits_width();
        page.click("Save");
        page.visible(CARD_ADDED);
        page.visible("Visa ••••4242");
        page.visible("Fixture Travel · Expires 07/34");
        page.visible("Never used on a page");
        page.fits_width();
        page.never_shows_a_full_number();

        page.click("Edit");
        page.visible("EDIT CARD");
        page.visible("Leave the number blank to keep Visa ••••4242.");
        page.never_shows_a_full_number();
        page.focus_field("card-nickname");
        page.type_text("Fixture Shopping");
        page.click("Save");
        page.visible(CARD_SAVED);
        page.visible("Fixture Shopping · Expires 07/34");

        page.h.reopen();
        profiles(&mut page, A);
        page.click("Unlock");
        assert!(page.appears("Add Password"), "a new window opens on passwords");
        page.click("Cards");
        page.visible("Visa ••••4242");
        page.visible("Fixture Shopping · Expires 07/34");

        page.click("Delete…");
        let prompt = "Delete the card “Visa ••••4242”? This can't be undone.";
        page.visible(prompt);
        page.fits_width();
        page.key(egui::Key::Escape, egui::Modifiers::NONE);
        page.visible(prompt);
        page.click("Cancel");
        assert!(!page.appears(prompt), "Cancel closes it at {size:?}");
        assert_eq!(page.h.passwords.vault().unwrap().cards().len(), 1, "Escape and Cancel delete nothing");
        page.click("Delete…");
        page.click("Delete");
        page.visible(CARD_DELETED);
        page.visible(EMPTY);
        assert!(page.h.passwords.vault().unwrap().cards().is_empty());
    }
}

#[test]
fn the_editor_has_no_security_code_and_its_hints_never_block_save() {
    for size in sizes() {
        let mut page = sheet(size);
        page.click("Unlock");
        page.click("Cards");
        page.click("Add Card");
        for label in ["Security code", "CVC", "CVV", "CSC", "SECURITY CODE"] {
            assert!(!page.appears(label), "{label} at {size:?}");
        }
        page.visible("The security code is never saved.");
        page.type_text("4242 4242 4242 4241");
        page.focus_field("card-month");
        page.type_text("1");
        page.focus_field("card-year");
        page.type_text("2020");
        page.visible(CardHint::FailsCheck.message());
        page.visible(CardHint::Expired.message());
        page.fits_width();
        page.click("Save");
        page.visible(CARD_ADDED);
        assert_eq!(page.h.passwords.vault().unwrap().cards().len(), 1, "saved as typed at {size:?}");

        page.click("Add Card");
        page.type_text("4242-abcd");
        page.focus_field("card-month");
        page.type_text("7");
        page.focus_field("card-year");
        page.type_text("2034");
        page.key(egui::Key::Enter, egui::Modifiers::NONE);
        page.visible("A card number has only digits.");
        page.fits_width();
        assert_eq!(page.h.passwords.page.card_editor.as_ref().unwrap().year.as_str(), "2034", "the draft is kept for a retry");
        page.focus_field("card-number");
        page.type_text("6767 6767 6767 6767 67");
        page.visible(CardHint::UnknownNetwork.message());
        page.key(egui::Key::Enter, egui::Modifiers::NONE);
        page.visible(CARD_ADDED);
        page.visible("Card ••••6767");
    }
}

#[test]
fn the_keyboard_reaches_the_cards_segment_and_every_card_action() {
    for size in sizes() {
        let mut page = sheet(size);
        page.click("Unlock");
        page.tab_to("Cards");
        page.key(egui::Key::Space, egui::Modifiers::NONE);
        page.settle();
        assert_eq!(page.h.passwords.page.section, Section::Cards, "the segment works from the keyboard at {size:?}");
        page.tab_to("Add Card");
        page.key(egui::Key::Enter, egui::Modifiers::NONE);
        page.settle();
        assert!(page.h.passwords.page.card_editor.is_some(), "Add Card works from the keyboard");
        page.type_text(MASTERCARD);
        page.focus_field("card-month");
        page.type_text("12");
        page.focus_field("card-year");
        page.type_text("2031");
        page.key(egui::Key::Enter, egui::Modifiers::NONE);
        page.visible(CARD_ADDED);
        page.tab_to("Edit");
        page.key(egui::Key::Enter, egui::Modifiers::NONE);
        page.settle();
        assert!(page.h.passwords.page.card_editor.as_ref().is_some_and(|editor| editor.id.is_some()), "Edit works from the keyboard");
        page.tab_to("Cancel");
        page.key(egui::Key::Enter, egui::Modifiers::NONE);
        page.settle();
        assert!(page.h.passwords.page.card_editor.is_none(), "Cancel closes the editor");
        page.tab_to("Delete…");
        page.key(egui::Key::Enter, egui::Modifiers::NONE);
        page.tab_to("Delete");
        page.key(egui::Key::Enter, egui::Modifiers::NONE);
        page.visible(CARD_DELETED);
    }
}

#[test]
fn escape_leaving_the_page_or_locking_drops_the_number_and_typing_it_holds_secure_input() {
    for size in sizes() {
        let mut page = sheet(size);
        page.account = Some("fixture-account".into());
        page.click("Unlock");
        page.click("Cards");
        page.click("Add Card");
        assert!(page.h.secure.held(), "typing a card number holds Secure Event Input at {size:?}");
        page.type_text(VISA);
        page.key(egui::Key::Escape, egui::Modifiers::NONE);
        assert!(!page.h.secure.held(), "Escape leaves the field and releases it");
        assert!(!page.settle().mentions(VISA), "Escape masks the number at {size:?}");
        page.frame_without_page();
        page.frame_without_page();
        page.settle();
        assert!(page.h.passwords.page.card_editor.is_none(), "leaving the page drops the draft at {size:?}");
        for name in crate::passwords::CARD_FIELD_IDS {
            assert!(egui::text_edit::TextEditState::load(&page.ctx, field_id(name)).is_none(), "{name} keeps no egui state");
        }
        page.click("Add Card");
        page.type_text(MASTERCARD);
        for name in crate::passwords::CARD_FIELD_IDS {
            let undo = egui::text_edit::TextEditState::load(&page.ctx, field_id(name)).map(|state| state.undoer());
            assert!(undo.is_none_or(|undo| !undo.has_undo(&Default::default()) && !undo.is_in_flux()), "{name} keeps undo history");
        }
        page.account = None;
        page.visible("Locked because you signed out.");
        assert!(!page.h.secure.held(), "locking releases it");
        assert!(page.h.passwords.page.card_editor.is_none(), "locking drops the draft");
        let calls = page.h.secure.calls();
        assert!(calls.chunks(2).all(|pair| pair == [true, false]), "every enable is matched by one disable: {calls:?}");
    }
}

/// What egui keeps between frames for field `id` that could hold its text:
/// its stored state, that state's undo and redo points, and whether an
/// undo point is pending. None when nothing is stored.
fn kept_for(page: &Sheet, id: Id) -> Option<String> {
    let state = egui::text_edit::TextEditState::load(&page.ctx, id)?;
    let undoer = state.undoer();
    Some(format!("{} {} pending {}", serde_json::to_string(&state).unwrap(), serde_json::to_string(&undoer).unwrap(), undoer.is_in_flux()))
}

/// egui keeps a field's undo history in its own stored state, apart from the
/// Zeroizing draft (team lead). While a number is typed, that history holds
/// nothing from one frame to the next; the number field's state is removed
/// once it loses focus, by Escape or a Save that is refused, and with the
/// whole editor on Cancel, Save, leaving the page and locking. No stored
/// state for the number field then holds any of its digits.
#[test]
fn no_stored_state_of_the_number_field_keeps_its_digits() {
    let number = card_field_id(crate::passwords::NUMBER);
    for size in sizes() {
        let mut page = sheet(size);
        page.account = Some("fixture-account".into());
        page.click("Unlock");
        page.click("Cards");
        page.click("Add Card");
        page.type_text(VISA);
        let kept = kept_for(&page, number);
        assert!(kept.as_deref().is_none_or(|kept| kept.contains(r#""undos":[]"#) && kept.contains(r#""redos":[]"#) && kept.ends_with("pending false") && !kept.contains("4242")),
            "while it types at {size:?}: {kept:?}");
        page.key(egui::Key::Escape, egui::Modifiers::NONE);
        assert_eq!(kept_for(&page, number), None, "Escape at {size:?}");
        page.focus_field("card-number");
        page.type_text(MASTERCARD);
        page.click("Cancel");
        assert_eq!(kept_for(&page, number), None, "Cancel at {size:?}");

        page.click("Add Card");
        page.type_text("4242-abcd");
        page.key(egui::Key::Enter, egui::Modifiers::NONE);
        page.settle();
        assert!(page.h.passwords.page.card_editor.as_ref().is_some_and(|editor| editor.error.is_some()), "a refused Save keeps the draft at {size:?}");
        assert_eq!(kept_for(&page, number), None, "a refused Save at {size:?}");
        page.focus_field("card-month");
        page.type_text("7");
        page.focus_field("card-year");
        page.type_text("2034");
        page.focus_field("card-number");
        page.type_text(VISA);
        page.key(egui::Key::Enter, egui::Modifiers::NONE);
        page.visible(CARD_ADDED);
        assert_eq!(kept_for(&page, number), None, "Save at {size:?}");

        page.click("Add Card");
        page.type_text(MASTERCARD);
        page.frame_without_page();
        page.frame_without_page();
        page.settle();
        assert_eq!(kept_for(&page, number), None, "leaving the page at {size:?}");
        page.click("Add Card");
        page.type_text(VISA);
        page.account = None;
        page.visible("Locked because you signed out.");
        assert_eq!(kept_for(&page, number), None, "locking at {size:?}");
    }
}

#[test]
fn the_list_follows_the_active_profile_and_a_damaged_card_can_only_be_deleted() {
    for size in sizes() {
        let mut page = sheet(size);
        page.click("Unlock");
        let vault = page.h.passwords.vault_mut().unwrap();
        vault.add_card(A, &card(VISA, "Fixture Travel")).unwrap();
        vault.add_card(B, &card(MASTERCARD, "Fixture Work")).unwrap();
        let broken = vault.add_card(A, &card(MASTERCARD, "Fixture Broken")).unwrap();
        page.click("Cards");
        page.visible("Visa ••••4242");
        assert!(!page.appears("Fixture Work · Expires 07/34"), "another profile's card isn't listed at {size:?}");
        profiles(&mut page, B);
        page.visible("Fixture Work · Expires 07/34");
        assert!(!page.appears("Visa ••••4242"));
        profiles(&mut page, A);

        page.click("Lock Now");
        page.vault_file().execute("UPDATE cards SET card = x'01' WHERE id = ?1", [&broken]).unwrap();
        page.click("Unlock");
        page.visible("Damaged card");
        page.visible(&VaultError::Unreadable.to_string());
        assert!(!page.appears("Fixture Broken · Expires 07/34"));
        page.fits_width();
        page.click_last("Delete…");
        page.visible("Delete this damaged card? This can't be undone.");
        page.click("Delete");
        page.visible(CARD_DELETED);
        assert!(!page.appears("Damaged card"));
        assert_eq!(page.h.passwords.vault().unwrap().cards().len(), 2, "only the damaged card was deleted");
    }
}

/// C10-3: the active profile changing, as the focused window's does, closes
/// an open Add Card or Edit Card form, wiping what was typed as Cancel does,
/// and a pending Delete prompt. Neither comes back on switching back, and
/// nothing typed is saved into either profile.
#[test]
fn a_profile_switch_closes_the_card_form_and_the_delete_prompt() {
    let titles = |page: &Sheet, profile: &str| page.h.passwords.vault().unwrap().cards_in(profile).iter().map(|saved| crate::passwords::card_title(saved)).collect::<Vec<_>>();
    let wiped = |page: &Sheet, case: &str| {
        assert!(page.h.passwords.page.card_editor.is_none() && !page.h.secure.held(), "{case}: the form closes at {:?}", page.size);
        for name in crate::passwords::CARD_FIELD_IDS {
            assert_eq!(kept_for(page, field_id(name)), None, "{case}: {name} keeps nothing at {:?}", page.size);
        }
    };
    for size in sizes() {
        let mut page = sheet(size);
        page.click("Unlock");
        page.click("Cards");
        page.click("Add Card");
        page.type_text(VISA);
        page.focus_field("card-month");
        page.type_text("7");
        page.focus_field("card-year");
        page.type_text("2034");
        page.focus_field("card-nickname");
        page.type_text("Fixture Typed In A");
        profiles(&mut page, B);
        page.visible(EMPTY);
        assert!(!page.appears("NEW CARD"), "Add Card closes at {size:?}");
        wiped(&page, "Add Card");
        profiles(&mut page, A);
        page.visible(EMPTY);
        assert!(!page.appears("NEW CARD"), "and doesn't come back at {size:?}");
        assert!(titles(&page, A).is_empty() && titles(&page, B).is_empty(), "nothing typed was saved at {size:?}");
        page.never_shows_a_full_number();

        let id = page.h.passwords.vault_mut().unwrap().add_card(A, &card(VISA, "Fixture A")).unwrap();
        page.h.passwords.vault_mut().unwrap().add_card(B, &card(MASTERCARD, "Fixture B")).unwrap();
        page.click("Edit");
        page.visible("EDIT CARD");
        page.focus_field("card-number");
        page.type_text(MASTERCARD);
        profiles(&mut page, B);
        page.visible("Fixture B · Expires 07/34");
        assert!(!page.appears("EDIT CARD"), "Edit Card closes at {size:?}");
        wiped(&page, "Edit Card");
        page.never_shows_a_full_number();
        profiles(&mut page, A);
        page.visible("Fixture A · Expires 07/34");
        assert!(!page.appears("EDIT CARD"), "and doesn't come back at {size:?}");
        assert_eq!(page.h.passwords.vault().unwrap().saved_card(&id).unwrap().last4.as_str(), "4242", "the edit wasn't saved at {size:?}");

        let prompt = "Delete the card “Visa ••••4242”? This can't be undone.";
        page.click("Delete…");
        page.visible(prompt);
        profiles(&mut page, B);
        page.visible("Fixture B · Expires 07/34");
        assert!(!page.appears(prompt) && page.h.passwords.page.confirm_delete_card.is_none(), "the prompt closes at {size:?}");
        profiles(&mut page, A);
        page.visible("Fixture A · Expires 07/34");
        assert!(!page.appears(prompt), "and doesn't come back at {size:?}");
        assert_eq!((titles(&page, A).len(), titles(&page, B).len()), (1, 1), "nothing was deleted at {size:?}");
    }
}

#[test]
fn reset_and_recover_count_cards_on_their_own() {
    for size in sizes() {
        let mut page = sheet(size);
        page.click("Unlock");
        page.add("one.example", "fixture", "fixture-password-one");
        page.h.passwords.vault_mut().unwrap().add_card(A, &card(VISA, "Fixture Travel")).unwrap();
        page.click("Lock Now");
        page.vault_file().execute_batch("ALTER TABLE entries RENAME COLUMN notes TO nktes").unwrap();
        page.h.reopen();
        profiles(&mut page, A);
        page.click("Recover Passwords…");
        page.visible(RECOVER_PROMPT);
        page.click("Recover Passwords");
        let name = moved_name(&page);
        page.visible(&format!("Recovered 1 of 1 password and 1 of 1 card. The damaged vault file is kept as {name} in Crossbow's data folder, readable only by your user account."));
        page.fits_width();
        page.click("Unlock");
        page.click("Cards");
        page.visible("Visa ••••4242");

        page.click("Lock Now");
        let id = page.h.passwords.vault().unwrap().id().to_owned();
        page.h.keys.forget(&id);
        page.click("Unlock");
        page.click("Reset Vault…");
        page.visible("Delete 1 saved password and 1 saved card from this vault file and start a new, empty vault with a new key? No existing key is deleted, since a copy of this vault, such as a backup, may still need it. This can't be undone.");
        page.fits_width();
    }
}

#[test]
fn a_profile_less_window_explains_that_cards_are_kept_per_profile() {
    for size in sizes() {
        let mut page = Sheet::new(size);
        page.click("Unlock");
        page.click("Cards");
        page.visible(crate::passwords::NO_CARD_PROFILE);
        page.fits_width();
        page.click("Add Card");
        assert!(page.h.passwords.page.card_editor.is_none(), "Add Card is disabled without a profile at {size:?}");
    }
}

/// `cargo test -p crossbow review_card_images -- --ignored`.
#[test]
#[ignore = "writes PNGs to target/lane-evidence/screens for manual review"]
fn review_card_images() {
    for size in sizes() {
        let mut page = sheet(size);
        page.click("Unlock");
        let vault = page.h.passwords.vault_mut().unwrap();
        vault.add_card(A, &card(VISA, "Fixture Travel")).unwrap();
        vault.add_card(A, &card(MASTERCARD, "")).unwrap();
        page.click("Cards");
        page.visible("Visa ••••4242");
        page.snapshot("cards-1-list");
        page.click("Add Card");
        page.type_text("4242 4242 4242 4241");
        page.visible(CardHint::FailsCheck.message());
        page.snapshot("cards-2-add-with-hint");
        page.click("Cancel");
        page.click("Delete…");
        page.snapshot("cards-3-delete");
    }
}
