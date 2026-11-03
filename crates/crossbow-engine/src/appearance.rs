//! Website appearance: whether a profile's pages are asked for their light
//! or dark version, as `prefers-color-scheme` reports it. Set through CEF's
//! Chrome color scheme on the profile's request context, which every page in
//! the profile shares; pages are never restyled.

use cef::*;
use std::cell::RefCell;
use std::collections::HashMap;

/// What a profile's pages are asked for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Appearance {
    /// Follow the system's light or dark mode.
    #[default]
    Automatic,
    Light,
    Dark,
}

thread_local! {
    /// Each profile's choice, for contexts made later.
    static CHOSEN: RefCell<HashMap<String, Appearance>> = RefCell::new(HashMap::new());
}

pub(crate) fn remember(profile_id: &str, appearance: Appearance) {
    CHOSEN.with_borrow_mut(|map| map.insert(profile_id.to_owned(), appearance));
}

/// The choice a new context for `profile_id` starts with.
pub(crate) fn chosen(profile_id: &str) -> Appearance {
    CHOSEN.with_borrow(|map| map.get(profile_id).copied().unwrap_or_default())
}

/// Only the mode: SYSTEM, LIGHT and DARK change light or dark, and leave
/// Chromium's colors as they are.
pub(crate) fn variant(appearance: Appearance) -> ColorVariant {
    match appearance {
        Appearance::Automatic => ColorVariant::SYSTEM,
        Appearance::Light => ColorVariant::LIGHT,
        Appearance::Dark => ColorVariant::DARK,
    }
}

pub(crate) fn apply(context: &RequestContext, appearance: Appearance) {
    // A transparent color keeps the default colors.
    context.set_chrome_color_scheme(variant(appearance), 0);
}

wrap_request_context_handler! {
    pub(crate) struct ContextAppearance {
        profile: String,
    }

    impl RequestContextHandler {
        fn on_request_context_initialized(&self, request_context: Option<&mut RequestContext>) {
            // Preferences are ready now. Read the latest choice in case it
            // changed while the context was being initialized.
            if let Some(context) = request_context { apply(context, chosen(&self.profile)); }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cef::rc::{ConvertReturnValue, RcImpl};
    use std::rc::Rc;

    type Calls = Rc<RefCell<Vec<(ColorVariant, u32)>>>;

    unsafe extern "C" fn record(raw: *mut sys::_cef_request_context_t, variant: sys::cef_color_variant_t, color: u32) {
        RcImpl::<sys::_cef_request_context_t, Calls>::get(raw).interface.borrow_mut().push((variant.into(), color));
    }

    fn context() -> (RequestContext, Calls) {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let mut raw: sys::_cef_request_context_t = unsafe { std::mem::zeroed() };
        raw.set_chrome_color_scheme = Some(record);
        let context = RcImpl::new(raw, calls.clone()).cast::<sys::_cef_request_context_t>().wrap_result();
        (context, calls)
    }

    #[test]
    fn a_context_receives_the_latest_choice_when_initialized_and_later_changes_use_cefs_setter() {
        let (mut a, a_calls) = context();
        let (mut b, b_calls) = context();
        remember("initializing-a", Appearance::Dark);
        let handler_a = ContextAppearance::new("initializing-a".into());
        let handler_b = ContextAppearance::new("initializing-b".into());
        // The choice changed while the context was being initialized.
        remember("initializing-a", Appearance::Light);
        handler_a.on_request_context_initialized(Some(&mut a));
        handler_b.on_request_context_initialized(Some(&mut b));
        assert_eq!(*a_calls.borrow(), [(ColorVariant::LIGHT, 0)]);
        assert_eq!(*b_calls.borrow(), [(ColorVariant::SYSTEM, 0)]);
        apply(&a, Appearance::Dark);
        apply(&a, Appearance::Automatic);
        assert_eq!(*a_calls.borrow(), [(ColorVariant::LIGHT, 0), (ColorVariant::DARK, 0), (ColorVariant::SYSTEM, 0)]);
        assert_eq!(b_calls.borrow().len(), 1, "the other context is untouched");
    }

    #[test]
    fn each_choice_is_one_of_cefs_three_modes_and_is_kept_per_profile() {
        assert_eq!(variant(Appearance::Automatic), ColorVariant::SYSTEM);
        assert_eq!(variant(Appearance::Light), ColorVariant::LIGHT);
        assert_eq!(variant(Appearance::Dark), ColorVariant::DARK);
        assert_eq!(chosen("fixture-never-set"), Appearance::Automatic, "a profile follows the system until chosen");
        remember("fixture-a", Appearance::Dark);
        remember("fixture-b", Appearance::Light);
        assert_eq!((chosen("fixture-a"), chosen("fixture-b")), (Appearance::Dark, Appearance::Light));
        remember("fixture-a", Appearance::Automatic);
        assert_eq!((chosen("fixture-a"), chosen("fixture-b")), (Appearance::Automatic, Appearance::Light));
    }
}
