//! Saved addresses through the controller, on a disposable vault with the
//! memory key store: the Settings list, fills and offers. Every address is a
//! synthetic fixture.

use super::*;
use crate::passwords::tests::Harness;

const SITE: &str = "https://shop.example";
const A: &str = "fixture-profile-a";
const B: &str = "fixture-profile-b";
const TAB: &str = "tab-1";
const STREET: &str = "1 Fixture Street\nUnit 2";
/// A page's parts, in the engine's order: name, organization, street, city,
/// region, postal code, country, phone, email.
const PARTS: [&str; 9] = ["Ada Fixture", "Fixture Ltd", STREET, "Testville", "Test Region", "75001", "FR", "+33 1 23 45 67 89", "ada@shop.example"];

fn harness() -> Harness {
    let mut h = Harness::new();
    h.passwords.set_profiles(A, [(A.to_owned(), "Personal".to_owned()), (B.to_owned(), "Work".to_owned())]);
    for profile in [A, B] { h.passwords.set_offer_to_save(profile, true); }
    h
}

fn unlocked() -> Harness {
    let mut h = harness();
    h.passwords.status().unwrap();
    h.passwords.unlock();
    assert!(h.passwords.unlocked());
    h
}

fn address(label: &str, parts: [&str; 9]) -> Address {
    let mut address = Address::default();
    address.set(AddressField::Label, label);
    for (field, part) in PAGE_PARTS.iter().zip(parts) { address.set(*field, part); }
    address
}

fn add(h: &mut Harness, profile: &str, label: &str) -> String {
    h.passwords.vault_mut().unwrap().add_address(profile, &address(label, PARTS)).unwrap()
}

/// Type into the open editor, as the page's fields would.
fn type_into(h: &mut Harness, field: AddressField, text: &str) {
    let editor = h.passwords.page.address_editor.as_mut().expect("the editor is open");
    let part = editor.address.part_mut(field);
    part.clear();
    part.push_str(text);
}

fn labels(h: &Harness) -> Vec<String> {
    h.passwords.listed_addresses().iter().map(|saved| saved.address.get(AddressField::Label).to_owned()).collect()
}

#[test]
fn addresses_are_added_edited_and_deleted_in_the_active_profile_and_survive_a_reopen() {
    let mut h = unlocked();
    h.passwords.start_add_address();
    let editor = h.passwords.page.address_editor.as_mut().unwrap();
    assert_eq!((editor.id.as_deref(), editor.profile.as_str(), editor.focus), (None, A, true), "Add saves into the active profile");
    for field in AddressField::ALL {
        assert!(editor.address.part_mut(field).capacity() >= field.limit() * 4, "{field:?} reserves room, so typing never reallocates");
    }
    type_into(&mut h, AddressField::Label, "Home");
    type_into(&mut h, AddressField::Street, STREET);
    type_into(&mut h, AddressField::City, "Testville");
    h.passwords.save_address_editor();
    assert!(h.passwords.page.address_editor.is_none());
    assert_eq!(h.passwords.page.notice.as_deref(), Some(ADDRESS_ADDED));
    let id = h.passwords.listed_addresses()[0].id.clone();
    assert_eq!(h.passwords.page.anchor.as_deref(), Some(id.as_str()), "the notice shows under the address");

    // Another profile's addresses are neither listed nor offered here.
    add(&mut h, B, "Office");
    assert_eq!(labels(&h), ["Home"]);
    h.passwords.set_profiles(B, [(A.to_owned(), "Personal".to_owned()), (B.to_owned(), "Work".to_owned())]);
    assert_eq!(labels(&h), ["Office"]);
    h.passwords.set_profiles(A, [(A.to_owned(), "Personal".to_owned()), (B.to_owned(), "Work".to_owned())]);

    h.passwords.start_edit_address(&id);
    let editor = h.passwords.page.address_editor.as_ref().unwrap();
    assert_eq!((editor.id.as_deref(), editor.address.get(AddressField::Street)), (Some(id.as_str()), STREET), "Edit opens the saved parts");
    type_into(&mut h, AddressField::City, "Newtown");
    h.passwords.save_address_editor();
    assert_eq!(h.passwords.page.notice.as_deref(), Some(ADDRESS_SAVED));

    h.reopen();
    h.passwords.set_profiles(A, [(A.to_owned(), "Personal".to_owned()), (B.to_owned(), "Work".to_owned())]);
    h.passwords.status().unwrap();
    assert!(h.passwords.listed_addresses().is_empty(), "nothing is read while locked");
    h.passwords.unlock();
    let saved = h.passwords.vault().unwrap().saved_address(&id).unwrap();
    assert_eq!((saved.profile.as_str(), saved.address.get(AddressField::City), saved.address.get(AddressField::Label)), (A, "Newtown", "Home"), "kept after a reopen");

    h.passwords.request_delete_address(&id);
    assert_eq!(h.passwords.page.confirm_delete_address.as_deref(), Some(id.as_str()), "Delete asks first");
    assert_eq!(labels(&h), ["Home"], "and deletes nothing yet");
    h.passwords.confirm_delete_address();
    assert_eq!(h.passwords.page.notice.as_deref(), Some(ADDRESS_DELETED));
    assert!(labels(&h).is_empty());
    h.reopen();
    h.passwords.status().unwrap();
    h.passwords.unlock();
    assert_eq!(h.passwords.vault().unwrap().addresses().len(), 1, "only the other profile's address is left");
}

#[test]
fn an_unusual_address_saves_with_hints_and_only_what_the_vault_cant_hold_is_refused() {
    let mut h = unlocked();
    h.passwords.start_add_address();
    type_into(&mut h, AddressField::Label, "Odd");
    h.passwords.save_address_editor();
    assert_eq!(h.passwords.page.address_editor.as_ref().unwrap().error.as_deref(), Some("Enter at least one part of the address."), "a label alone isn't an address");

    type_into(&mut h, AddressField::City, &"é".repeat(129));
    h.passwords.save_address_editor();
    let editor = h.passwords.page.address_editor.as_ref().expect("the draft is kept for a correction");
    assert_eq!(editor.error.as_deref(), Some("That city is too long."), "258 bytes is over the city's 256");
    assert_eq!(editor.address.get(AddressField::Label), "Odd");

    // No street, no postal code, an email without @ and a phone without digits.
    type_into(&mut h, AddressField::City, "Ünïcödé-sur-Mer");
    type_into(&mut h, AddressField::Email, "ada at shop");
    type_into(&mut h, AddressField::Phone, "ask reception");
    type_into(&mut h, AddressField::Country, "Atlantis");
    let hints = address_hints(&h.passwords.page.address_editor.as_ref().unwrap().address);
    assert_eq!(hints.iter().map(|(field, _)| *field).collect::<Vec<_>>(), [AddressField::Email, AddressField::Phone]);
    h.passwords.save_address_editor();
    assert_eq!(h.passwords.page.notice.as_deref(), Some(ADDRESS_ADDED), "hints never block a save");
    let saved = &h.passwords.listed_addresses()[0].address;
    assert_eq!((saved.get(AddressField::Email), saved.get(AddressField::Phone), saved.get(AddressField::City)), ("ada at shop", "ask reception", "Ünïcödé-sur-Mer"), "saved as typed");
}

#[test]
fn locking_or_leaving_the_page_drops_every_opened_address_and_the_draft() {
    let mut h = unlocked();
    let id = add(&mut h, A, "Home");
    h.passwords.start_edit_address(&id);
    h.passwords.tick_at(None, false);
    assert!(h.passwords.page.address_editor.is_none(), "leaving the page drops the draft");
    h.passwords.start_edit_address(&id);
    h.passwords.request_delete_address(&id);
    h.passwords.start_edit_address(&id);
    h.passwords.lock(None);
    assert!(h.passwords.page.address_editor.is_none() && h.passwords.page.confirm_delete_address.is_none());
    assert!(h.passwords.vault().unwrap().addresses().is_empty(), "no address stays open in memory while locked");
    assert!(h.passwords.addresses_for(A).is_empty());
    assert_eq!(h.passwords.address_to_fill(&id, A).err(), Some(ADDRESS_GONE));
}

#[test]
fn a_fill_takes_only_an_address_of_the_page_profile_and_records_the_use() {
    let mut h = unlocked();
    let home = add(&mut h, A, "Home");
    let office = add(&mut h, B, "Office");
    assert_eq!(h.passwords.addresses_for(A), [(home.clone(), "Home".to_owned())]);
    let parts = h.passwords.address_to_fill(&home, A).unwrap();
    assert_eq!(parts.each_ref().map(|part| part.as_str()), PARTS, "every part but the label, in the engine's order");
    assert!(h.passwords.vault().unwrap().saved_address(&home).unwrap().used_ms.is_some(), "the fill is recorded");
    assert_eq!(h.passwords.address_to_fill(&office, A).err(), Some(ADDRESS_GONE), "never another profile's address");
    h.passwords.vault_mut().unwrap().delete_address(&home).unwrap();
    assert_eq!(h.passwords.address_to_fill(&home, A).err(), Some(ADDRESS_GONE));
}

#[test]
fn the_key_shows_for_an_address_field_with_addresses_in_its_profile_or_while_locked() {
    let mut h = unlocked();
    assert_eq!(h.passwords.address_key_state(A), None, "no addresses, no key");
    add(&mut h, A, "Home");
    add(&mut h, A, "Office");
    assert_eq!(h.passwords.address_key_state(A), Some(KeyState::Addresses(2)));
    assert_eq!(h.passwords.address_key_state(B), None, "another profile's don't count");
    h.passwords.lock(None);
    assert_eq!(h.passwords.address_key_state(B), Some(KeyState::Locked), "locked, any address may be this profile's");
}

#[test]
fn an_address_is_offered_only_with_the_switch_on_and_a_street_with_a_city_or_postal_code() {
    let mut h = unlocked();
    h.passwords.set_offer_to_save(A, false);
    h.passwords.offer_address(TAB, A, SITE, PARTS);
    assert!(h.passwords.address_offer(TAB).is_none(), "off by default, and off here");
    h.passwords.set_offer_to_save(A, true);
    let with = |changes: &[(usize, &'static str)]| {
        let mut parts = PARTS;
        for (index, value) in changes { parts[*index] = value; }
        parts
    };
    for (case, parts) in [("no street", with(&[(2, "")])), ("no city or postal code", with(&[(3, ""), (5, "")])), ("blank street lines", with(&[(2, " \n ")]))] {
        h.passwords.offer_address(TAB, A, SITE, parts);
        assert!(h.passwords.address_offer(TAB).is_none(), "{case}");
    }
    for (case, origin, profile) in [("an opaque origin", "null", A), ("a file page", "file:///tmp", A), ("a tab without a profile", SITE, "")] {
        h.passwords.offer_address(TAB, profile, origin, PARTS);
        assert!(h.passwords.address_offer(TAB).is_none(), "{case}");
    }
    let long: &'static str = format!("1 Fixture Street {}", "x".repeat(AddressField::Street.limit())).leak();
    h.passwords.offer_address(TAB, A, SITE, with(&[(2, long)]));
    assert!(h.passwords.address_offer(TAB).is_none(), "a part over its limit");

    h.passwords.offer_address(TAB, A, SITE, with(&[(3, "")]));
    let offer = h.passwords.address_offer(TAB).expect("a street and a postal code are enough");
    assert_eq!((offer.origin.as_str(), offer.profile.as_str(), offer.address.get(AddressField::Label)), (SITE, A, ""));
    let first = offer.revision;
    h.passwords.offer_address(TAB, A, SITE, PARTS);
    assert!(h.passwords.address_offer(TAB).unwrap().revision > first, "one per tab: the newer replaces it");
    h.passwords.offer_address("tab-2", B, SITE, PARTS);
    assert_eq!(h.passwords.address_offer_tabs().count(), 2);
    h.passwords.keep_offers(|tab| tab == TAB);
    assert_eq!(h.passwords.address_offer_tabs().collect::<Vec<_>>(), [TAB], "a closed tab's offer goes");
    h.passwords.set_offer_to_save(A, false);
    assert!(h.passwords.address_offer(TAB).is_none(), "turning the switch off wipes it");
}

#[test]
fn offers_are_wiped_after_two_minutes_and_when_the_account_changes() {
    let mut h = unlocked();
    h.passwords.tick_at(Some("account-a"), false);
    h.passwords.offer_address(TAB, A, SITE, PARTS);
    let wait = h.passwords.tick_at(Some("account-a"), false).unwrap();
    assert!(wait <= OFFER_TIMEOUT + Duration::from_millis(50), "a tick is due when it expires: {wait:?}");
    h.advance(OFFER_TIMEOUT - Duration::from_secs(1));
    h.passwords.tick_at(Some("account-a"), false);
    assert!(h.passwords.address_offer(TAB).is_some());
    h.advance(Duration::from_secs(1));
    h.passwords.tick_at(Some("account-a"), false);
    assert!(h.passwords.address_offer(TAB).is_none(), "gone after two minutes");
    h.passwords.offer_address(TAB, A, SITE, PARTS);
    h.passwords.tick_at(Some("account-b"), false);
    assert!(h.passwords.address_offer(TAB).is_none(), "and when the account changes");
}

#[test]
fn save_while_locked_unlocks_first_and_an_address_already_saved_is_not_saved_twice() {
    let mut h = unlocked();
    h.passwords.lock(None);
    h.passwords.offer_address(TAB, A, SITE, PARTS);
    h.passwords.address_offer_mut(TAB).unwrap().address.set(AddressField::Label, "From the shop");
    assert_eq!(h.passwords.save_address_offer(TAB), None);
    assert!(h.passwords.address_offer(TAB).unwrap().unlocking, "Save asks to unlock first");
    h.keys.fail_reads(true);
    assert_eq!(h.passwords.unlock_for_address_offer(TAB), None);
    assert!(h.passwords.address_offer(TAB).unwrap().error.is_some(), "a failed unlock says why and keeps the offer");
    h.keys.fail_reads(false);
    assert_eq!(h.passwords.unlock_for_address_offer(TAB), Some(ADDRESS_SAVED));
    assert!(h.passwords.address_offer(TAB).is_none());
    assert_eq!(h.passwords.addresses_for(A), [(h.passwords.vault().unwrap().addresses_in(A)[0].id.clone(), "From the shop".to_owned())], "saved in the tab's profile with its label");

    h.passwords.offer_address(TAB, A, SITE, PARTS);
    assert!(h.passwords.address_offer(TAB).is_none(), "an address already saved in the profile isn't offered");
    h.passwords.offer_address(TAB, B, SITE, PARTS);
    assert!(h.passwords.address_offer(TAB).is_some(), "in another profile it is");
    h.passwords.vault_mut().unwrap().add_address(B, &address("", PARTS)).unwrap();
    assert_eq!(h.passwords.save_address_offer(TAB), Some(ADDRESS_ALREADY_SAVED), "saved meanwhile: nothing is added");
    assert_eq!(h.passwords.vault().unwrap().addresses_in(B).len(), 1);
}

#[test]
fn never_for_this_site_shares_the_profile_never_save_list_with_passwords() {
    let mut h = unlocked();
    h.passwords.offer_address(TAB, A, SITE, PARTS);
    assert_eq!(h.passwords.never_for_address_offer(TAB), Some(super::NEVER_SAVED));
    assert!(h.passwords.address_offer(TAB).is_none());
    assert_eq!(h.passwords.never_saved(A), [SITE]);
    h.passwords.offer_address(TAB, A, SITE, PARTS);
    h.passwords.offer_login(TAB, A, SITE, "fixture-user", "fixture-page-password-1");
    assert!(h.passwords.address_offer(TAB).is_none() && h.passwords.offer(TAB).is_none(), "neither an address nor a login is offered there");
    h.passwords.offer_address(TAB, B, SITE, PARTS);
    assert!(h.passwords.address_offer(TAB).is_some(), "the list is per profile");
    h.passwords.dismiss_address_offer(TAB);
    assert!(h.passwords.address_offer(TAB).is_none() && h.passwords.vault().unwrap().addresses().is_empty(), "Not Now saves nothing");
}

#[test]
fn reset_counts_addresses_and_recover_reports_them_on_their_own() {
    let mut h = unlocked();
    h.passwords.vault_mut().unwrap().add(&EntryInput { origin: SITE, username: "fixture-user", password: "fixture-password", notes: "", profile: Some(A) }).unwrap();
    add(&mut h, A, "Home");
    add(&mut h, B, "Office");
    assert_eq!(h.passwords.reset_scope(), ResetScope::Entries { passwords: 1, addresses: 2, cards: 0 });
    h.passwords.lock(None);
    assert_eq!(h.passwords.reset_scope(), ResetScope::Entries { passwords: 1, addresses: 2, cards: 0 }, "counted while locked too");

    let counted = |recovered, found, complete| crossbow_core::vault::Counted { recovered, found, complete };
    let recovery = |addresses, never_save| Recovery { recovered: 3, found: 4, complete: true, addresses, cards: counted(0, 0, true), never_save, moved: "vault.damaged.sqlite".into(), also_kept: vec![] };
    let kept = "kept as vault.damaged.sqlite in Crossbow's data folder, readable only by your user account.";
    assert_eq!(recovered(&recovery(counted(0, 0, true), counted(0, 0, true))), format!("Recovered 3 of 4 passwords. 1 couldn't be read; the damaged file is {kept}"), "without addresses it reads as before");
    assert_eq!(recovered(&recovery(counted(2, 2, true), counted(1, 1, true))), format!("Recovered 3 of 4 passwords, 2 of 2 addresses and 1 of 1 never-save site. 1 couldn't be read; the damaged file is {kept}"));
    assert_eq!(recovered(&recovery(counted(0, 1, false), counted(0, 0, true))), format!("Recovered 3 of 4 passwords and 0 of 1 address. The addresses couldn't be read in full. 1 password and 1 address couldn't be read; the damaged file is {kept}"));
    let all = Recovery { recovered: 1, found: 1, complete: true, addresses: counted(1, 3, true), cards: counted(0, 0, true), never_save: counted(0, 2, true), moved: "vault.damaged.sqlite".into(), also_kept: vec![] };
    assert_eq!(recovered(&all), format!("Recovered 1 of 1 password, 1 of 3 addresses and 0 of 2 never-save sites. 2 addresses and 2 never-save sites couldn't be read; the damaged file is {kept}"));
}

#[test]
fn no_part_label_or_site_of_an_address_reaches_the_log_or_debug_output() {
    let capture = crate::passwords::page_logins::tests::captured_logs();
    let mut h = unlocked();
    let id = add(&mut h, A, "Fixture-Label-Home");
    h.passwords.start_edit_address(&id);
    h.passwords.save_address_editor();
    let _ = h.passwords.address_to_fill(&id, A);
    let _ = h.passwords.address_to_fill(&id, B);
    h.passwords.lock(None);
    h.passwords.offer_address(TAB, A, "https://fixture-shop.example", PARTS);
    h.passwords.save_address_offer(TAB);
    h.passwords.unlock_for_address_offer(TAB);
    h.passwords.offer_address("tab-2", B, "https://fixture-shop.example", PARTS);
    h.passwords.address_offer_mut("tab-2").unwrap().address.set(AddressField::Label, "Fixture-Label-Shop");
    let printed = format!("{:?} {:?} {:?} {:#?}", h.passwords.address_offer("tab-2"), h.passwords.page.address_editor, h.passwords.vault().unwrap().addresses(), h.passwords.address_offers);
    h.passwords.never_for_address_offer("tab-2");
    let lines = capture.0.lock().unwrap().clone();
    for secret in PARTS.iter().flat_map(|part| part.lines()).chain(["Fixture-Label-Home", "Fixture-Label-Shop", "fixture-shop.example"]) {
        assert!(lines.iter().all(|line| !line.contains(secret)), "{secret:?} was logged: {lines:?}");
        assert!(!printed.contains(secret), "{secret:?} is in debug output: {printed}");
    }
}

#[test]
fn addresses_are_listed_and_offered_by_their_title() {
    let mut h = unlocked();
    add(&mut h, A, "office");
    let mut unlabeled = address("", PARTS);
    unlabeled.set(AddressField::Name, "");
    h.passwords.vault_mut().unwrap().add_address(A, &unlabeled).unwrap();
    add(&mut h, A, "Home");
    let titles: Vec<String> = h.passwords.addresses_for(A).into_iter().map(|(_, title)| title).collect();
    assert_eq!(titles, ["1 Fixture Street", "Home", "office"], "a label, else the name, else the first street line; case aside");
    assert_eq!(h.passwords.listed_addresses().iter().map(|saved| address_title(&saved.address)).collect::<Vec<_>>(), titles);
}
