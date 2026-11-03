//! Settings → Passwords → Addresses driven by simulated pointer and keyboard
//! input in the real Settings sheet geometry, at 640×420 and 1360×860, on a
//! disposable vault with the memory key store. Every address is a synthetic
//! fixture.

use super::*;
use crate::passwords::{ADDRESS_ADDED, ADDRESS_DELETED, ADDRESS_SAVED, Section, address_field_id};
use crossbow_core::vault::{Address, AddressField};

const A: &str = "fixture-profile-a";
const B: &str = "fixture-profile-b";
const EMPTY: &str = "No saved addresses in this profile yet.";

fn profiles(page: &mut Sheet, current: &str) {
    page.h.passwords.set_profiles(current, [(A.to_owned(), "Personal".to_owned()), (B.to_owned(), "Work".to_owned())]);
}

fn sheet(size: Vec2) -> Sheet {
    let mut page = Sheet::new(size);
    profiles(&mut page, A);
    page
}

fn saved(label: &str, street: &str, city: &str) -> Address {
    let mut address = Address::default();
    address.set(AddressField::Label, label);
    address.set(AddressField::Street, street);
    address.set(AddressField::City, city);
    address
}

/// The one address saved in `profile`, with its id.
fn only(page: &Sheet, profile: &str) -> (String, Address) {
    let vault = page.h.passwords.vault().unwrap();
    let listed = vault.addresses_in(profile);
    assert_eq!(listed.len(), 1, "one address in {profile}");
    (listed[0].id.clone(), listed[0].address.clone())
}

#[test]
fn addresses_are_added_edited_and_deleted_with_the_pointer_and_kept_after_a_reopen() {
    for size in sizes() {
        let mut page = sheet(size);
        page.click("Unlock");
        page.click("Addresses");
        assert_eq!(page.h.passwords.page.section, Section::Addresses);
        page.visible(EMPTY);
        assert!(!page.appears("Add Password"), "the password list is hidden at {size:?}");
        page.fits_width();
        page.click("Add Address");
        assert_eq!(page.focused(), Some(address_field_id(AddressField::Label)), "Add puts focus on the label at {size:?}");
        page.type_text("Fixture Home");
        page.focus_field("address-street");
        page.type_text("1 Fixture Street");
        page.key(egui::Key::Enter, egui::Modifiers::NONE);
        page.frame(vec![egui::Event::Text("Unit 2".into())]);
        page.focus_field("address-city");
        page.type_text("Testville");
        page.focus_field("address-postal-code");
        page.type_text("75001");
        page.visible("NEW ADDRESS");
        page.fits_width();
        page.click("Save");
        page.visible(ADDRESS_ADDED);
        page.visible("Fixture Home");
        page.visible("1 Fixture Street, Unit 2, Testville, 75001");
        page.visible("Never used on a page");
        page.fits_width();
        let (id, address) = only(&page, A);
        assert_eq!((address.get(AddressField::Street), address.get(AddressField::PostalCode)), ("1 Fixture Street\nUnit 2", "75001"));

        page.click("Edit");
        page.visible("EDIT ADDRESS");
        page.focus_field("address-city");
        page.type_text("Newtown");
        page.click("Save");
        page.visible(ADDRESS_SAVED);
        page.visible("1 Fixture Street, Unit 2, Newtown, 75001");

        page.h.reopen();
        profiles(&mut page, A);
        page.click("Unlock");
        assert!(page.appears("Add Password"), "a new window opens on passwords");
        page.click("Addresses");
        page.visible("1 Fixture Street, Unit 2, Newtown, 75001");

        page.click("Delete…");
        let prompt = "Delete the address “Fixture Home”? This can't be undone.";
        page.visible(prompt);
        page.fits_width();
        page.key(egui::Key::Escape, egui::Modifiers::NONE);
        page.visible(prompt);
        page.click("Cancel");
        assert!(!page.appears(prompt), "Cancel closes it at {size:?}");
        assert_eq!(only(&page, A).0, id, "Escape and Cancel delete nothing");
        page.click("Delete…");
        page.click("Delete");
        page.visible(ADDRESS_DELETED);
        page.visible(EMPTY);
        assert!(page.h.passwords.vault().unwrap().addresses().is_empty());
    }
}

#[test]
fn the_keyboard_reaches_the_switch_and_every_address_action_and_an_error_keeps_the_draft() {
    for size in sizes() {
        let mut page = sheet(size);
        page.click("Unlock");
        page.tab_to("Addresses");
        page.key(egui::Key::Space, egui::Modifiers::NONE);
        page.settle();
        assert_eq!(page.h.passwords.page.section, Section::Addresses, "the switch works from the keyboard at {size:?}");
        page.tab_to("Add Address");
        page.key(egui::Key::Enter, egui::Modifiers::NONE);
        page.settle();
        assert!(page.h.passwords.page.address_editor.is_some(), "Add Address works from the keyboard");
        page.type_text("Only a label");
        page.key(egui::Key::Enter, egui::Modifiers::NONE);
        page.visible("Enter at least one part of the address.");
        page.fits_width();
        assert_eq!(page.h.passwords.page.address_editor.as_ref().unwrap().address.get(AddressField::Label), "Only a label", "the draft is kept for a retry");
        page.focus_field("address-city");
        page.type_text("Testville");
        page.key(egui::Key::Enter, egui::Modifiers::NONE);
        page.visible(ADDRESS_ADDED);
        page.tab_to("Edit");
        page.key(egui::Key::Enter, egui::Modifiers::NONE);
        page.settle();
        assert!(page.h.passwords.page.address_editor.as_ref().is_some_and(|editor| editor.id.is_some()), "Edit works from the keyboard");
        page.tab_to("Cancel");
        page.key(egui::Key::Enter, egui::Modifiers::NONE);
        page.settle();
        assert!(page.h.passwords.page.address_editor.is_none(), "Cancel closes the editor");
        page.tab_to("Delete…");
        page.key(egui::Key::Enter, egui::Modifiers::NONE);
        page.tab_to("Delete");
        page.key(egui::Key::Enter, egui::Modifiers::NONE);
        page.visible(ADDRESS_DELETED);
        page.tab_to("Passwords");
        page.key(egui::Key::Space, egui::Modifiers::NONE);
        page.visible("No saved passwords yet.");
    }
}

#[test]
fn an_unusual_address_saves_with_a_hint_and_an_overlong_part_is_explained() {
    for size in sizes() {
        let mut page = sheet(size);
        page.click("Unlock");
        page.click("Addresses");
        page.click("Add Address");
        page.focus_field("address-email");
        page.type_text("ada at shop");
        page.focus_field("address-phone");
        page.type_text("ask reception");
        page.visible("This email address has no @. It will be saved as typed.");
        page.visible("This phone number has no digits. It will be saved as typed.");
        page.fits_width();
        page.focus_field("address-city");
        // 200 characters fit the field, but at two bytes each they are over the city's 256 bytes.
        page.type_text(&"é".repeat(200));
        page.click("Save");
        // The field's own text scrolls inside it; the explanation must fit.
        let shown = page.visible("That city is too long.");
        let (_, bounds, clip) = shown.find("That city is too long.", false).unwrap();
        assert!(clip.expand(0.5).contains_rect(*bounds), "the error fits at {size:?}");
        page.focus_field("address-city");
        page.type_text("Ünïcödé-sur-Mer");
        page.click("Save");
        page.visible(ADDRESS_ADDED);
        let (_, address) = only(&page, A);
        assert_eq!((address.get(AddressField::Email), address.get(AddressField::Phone)), ("ada at shop", "ask reception"), "saved as typed at {size:?}");
    }
}

#[test]
fn the_list_follows_the_active_profile_and_a_damaged_address_can_only_be_deleted() {
    for size in sizes() {
        let mut page = sheet(size);
        page.click("Unlock");
        let vault = page.h.passwords.vault_mut().unwrap();
        vault.add_address(A, &saved("Fixture Home", "1 Fixture Street", "Testville")).unwrap();
        vault.add_address(B, &saved("Fixture Office", "9 Work Road", "Worktown")).unwrap();
        let broken = vault.add_address(A, &saved("Fixture Broken", "5 Broken Lane", "Rubble")).unwrap();
        page.click("Addresses");
        page.visible("Fixture Home");
        assert!(!page.appears("Fixture Office"), "another profile's address isn't listed at {size:?}");
        profiles(&mut page, B);
        page.visible("Fixture Office");
        assert!(!page.appears("Fixture Home"));
        profiles(&mut page, A);

        page.click("Lock Now");
        page.vault_file().execute("UPDATE addresses SET address = x'01' WHERE id = ?1", [&broken]).unwrap();
        page.click("Unlock");
        page.visible("Damaged address");
        page.visible(&VaultError::Unreadable.to_string());
        assert!(!page.appears("Fixture Broken") && !page.appears("Rubble"));
        page.fits_width();
        assert_eq!(page.h.passwords.vault().unwrap().addresses().last().map(|saved| saved.id.as_str()), Some(broken.as_str()), "damaged rows sort last");
        page.click_last("Delete…");
        page.visible("Delete this damaged address? This can't be undone.");
        page.click("Delete");
        page.visible(ADDRESS_DELETED);
        assert!(!page.appears("Damaged address"));
        assert_eq!(page.h.passwords.vault().unwrap().addresses().len(), 2, "only the damaged address was deleted");
    }
}

#[test]
fn leaving_the_page_or_locking_drops_the_address_draft_and_its_field_state() {
    for size in sizes() {
        let mut page = sheet(size);
        page.account = Some("fixture-account".into());
        page.click("Unlock");
        page.click("Addresses");
        page.click("Add Address");
        page.focus_field("address-street");
        page.type_text("7 Draft Street");
        page.frame_without_page();
        page.frame_without_page();
        page.settle();
        assert!(page.h.passwords.page.address_editor.is_none(), "leaving the page drops the draft at {size:?}");
        for name in crate::passwords::ADDRESS_FIELD_IDS {
            assert!(egui::text_edit::TextEditState::load(&page.ctx, field_id(name)).is_none(), "{name} keeps no egui state");
        }
        assert!(!page.appears("7 Draft Street"));
        page.click("Add Address");
        page.focus_field("address-street");
        page.type_text("8 Draft Street");
        for name in crate::passwords::ADDRESS_FIELD_IDS {
            let undo = egui::text_edit::TextEditState::load(&page.ctx, field_id(name)).map(|state| state.undoer());
            assert!(undo.is_none_or(|undo| !undo.has_undo(&Default::default()) && !undo.is_in_flux()), "{name} keeps undo history");
        }
        page.account = None;
        page.visible("Locked because you signed out.");
        assert!(page.h.passwords.page.address_editor.is_none() && page.h.passwords.vault().unwrap().addresses().is_empty(), "locking drops the draft and every opened address");
        assert!(!page.appears("8 Draft Street"));
    }
}

#[test]
fn reset_says_how_many_addresses_it_deletes() {
    for size in sizes() {
        let mut page = sheet(size);
        page.click("Unlock");
        page.add("example.com", "fixture", "fixture-password-reset");
        let vault = page.h.passwords.vault_mut().unwrap();
        vault.add_address(A, &saved("Fixture Home", "1 Fixture Street", "Testville")).unwrap();
        vault.add_address(B, &saved("Fixture Office", "9 Work Road", "Worktown")).unwrap();
        page.click("Lock Now");
        let id = page.h.passwords.vault().unwrap().id().to_owned();
        page.h.keys.forget(&id);
        page.click("Unlock");
        page.click("Reset Vault…");
        page.visible("Delete 1 saved password and 2 saved addresses from this vault file and start a new, empty vault with a new key? No existing key is deleted, since a copy of this vault, such as a backup, may still need it. This can't be undone.");
        page.fits_width();
        page.click("Delete All and Reset");
        page.click("Unlock");
        page.click("Addresses");
        page.visible(EMPTY);
    }
}

#[test]
fn recover_on_the_page_brings_addresses_back_and_counts_them_on_their_own() {
    for size in sizes() {
        let mut page = sheet(size);
        page.click("Unlock");
        page.add("one.example", "fixture", "fixture-password-one");
        page.h.passwords.vault_mut().unwrap().add_address(A, &saved("Fixture Home", "1 Fixture Street", "Testville")).unwrap();
        page.click("Lock Now");
        page.vault_file().execute_batch("ALTER TABLE entries RENAME COLUMN notes TO nktes").unwrap();
        page.h.reopen();
        profiles(&mut page, A);
        page.click("Recover Passwords…");
        page.visible(RECOVER_PROMPT);
        page.click("Recover Passwords");
        let name = moved_name(&page);
        page.visible(&format!("Recovered 1 of 1 password and 1 of 1 address. The damaged vault file is kept as {name} in Crossbow's data folder, readable only by your user account."));
        page.fits_width();
        page.click("Unlock");
        page.click("Addresses");
        page.visible("Fixture Home");
        page.visible("1 Fixture Street, Testville");
    }
}

#[test]
fn a_profile_less_window_explains_that_addresses_are_kept_per_profile() {
    for size in sizes() {
        let mut page = Sheet::new(size);
        page.click("Unlock");
        page.click("Addresses");
        page.visible(crate::passwords::NO_PROFILE);
        page.fits_width();
        page.click("Add Address");
        assert!(page.h.passwords.page.address_editor.is_none(), "Add Address is disabled without a profile at {size:?}");
    }
}

/// `cargo test -p crossbow review_address_images -- --ignored`.
#[test]
#[ignore = "writes PNGs to target/lane-evidence/screens for manual review"]
fn review_address_images() {
    for size in sizes() {
        let mut page = sheet(size);
        page.click("Unlock");
        let vault = page.h.passwords.vault_mut().unwrap();
        let mut home = saved("Home", "1 Fixture Street\nUnit 2", "Testville");
        home.set(AddressField::PostalCode, "75001");
        home.set(AddressField::Country, "FR");
        vault.add_address(A, &home).unwrap();
        vault.add_address(A, &saved("", "42 Very Long Fixture Boulevard of the Synthetic Republic, Building C, Staircase 4", "Longville-sur-Fixture")).unwrap();
        page.click("Addresses");
        page.visible("Home");
        page.snapshot("addresses-1-list");
        page.click("Add Address");
        page.focus_field("address-email");
        page.type_text("ada at shop");
        page.visible("This email address has no @. It will be saved as typed.");
        page.snapshot("addresses-2-add-with-hint");
        page.click("Cancel");
        page.click("Delete…");
        page.snapshot("addresses-3-delete");
    }
}

