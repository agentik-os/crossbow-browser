//! Ask AI in the Command Bar. With it on, typed text offers an Ask row; Tab
//! enters Ask mode and Return opens Ask AI with that text as the first
//! message, sent once. Off by default, it uses the consent in `ai_features`,
//! and the command bar's never-send rules refuse addresses, paths and secrets.

use crate::ai_features::{self, Ready};
use crossbow_ai::chat::MAX_PROMPT_BYTES;
use crossbow_core::library::Library;
use crossbow_core::suggest::{Refusal, refusal};

pub const FEATURE: &str = "ask_command_bar";
pub const NAME: &str = "Ask AI in the Command Bar";

/// The provider the Ask row names: the one this profile consented to, while
/// the feature is on.
pub fn offered(lib: &Library, profile: &str) -> Option<&'static str> {
    if !lib.profiles.contains_key(profile) { return None; }
    ai_features::consented(lib, profile, FEATURE).ok().flatten().map(|ready| ready.provider())
}

/// Where typed `text` may be sent now, or why it is not sent, in one line.
pub fn check(lib: &Library, profile: &str, text: &str) -> Result<Ready, String> {
    let text = text.trim();
    let refused = match refusal(text) {
        _ if text.is_empty() => Some("Type a question first."),
        Some(Refusal::Address) => Some("Not sent: it looks like a web address."),
        Some(Refusal::Path) => Some("Not sent: it looks like a file path."),
        Some(Refusal::Secret) => Some("Not sent: it looks like a password, key or personal detail."),
        None if text.len() > MAX_PROMPT_BYTES => Some("Not sent: a question can be up to 16,384 bytes."),
        None if text.chars().any(char::is_control) => Some("Not sent: it has control characters."),
        None => None,
    };
    if let Some(reason) = refused { return Err(reason.into()); }
    ai_features::allowed(lib, profile, FEATURE, NAME)
}

// The length reason states the limit in words.
const _: () = assert!(MAX_PROMPT_BYTES == 16_384);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_features::tests::{configured, fixture_ready};
    use crossbow_ai::local::LocalEndpoint;
    use crossbow_core::store::Store;

    #[test]
    fn questions_need_the_feature_on_and_break_no_never_send_rule() {
        let (lib, _, profile) = configured(Store::open_in_memory().unwrap(), LocalEndpoint { port: 41_310, ..Default::default() });
        assert_eq!(offered(&lib, &profile), None, "off by default");
        assert_eq!(check(&lib, &profile, "what is rust"), Err("Ask AI in the Command Bar is off. Turn it on in Settings → AI Providers.".into()));
        ai_features::enable(&lib, &profile, FEATURE, &fixture_ready(41_310)).unwrap();
        assert_eq!(offered(&lib, &profile), Some("Ollama"));
        assert_eq!(check(&lib, &profile, "  what is rust  "), Ok(fixture_ready(41_310)));
        for (text, reason) in [
            ("", "Type a question first."),
            ("   ", "Type a question first."),
            ("github.com/rust-lang", "Not sent: it looks like a web address."),
            ("https://example.test/a b", "Not sent: it looks like a web address."),
            ("~/notes/plan.txt", "Not sent: it looks like a file path."),
            ("my password is hunter2", "Not sent: it looks like a password, key or personal detail."),
            ("mail me@example.test", "Not sent: it looks like a password, key or personal detail."),
            ("sk-live-0123456789abcdef", "Not sent: it looks like a password, key or personal detail."),
            (&"why ".repeat(5_000), "Not sent: a question can be up to 16,384 bytes."),
            ("two\u{0}lines", "Not sent: it has control characters."),
        ] {
            assert_eq!(check(&lib, &profile, text), Err(reason.into()), "{:?}", &text[..text.len().min(40)]);
        }
        // A long question that breaks no rule is not refused for being longer than a search.
        assert!(check(&lib, &profile, &"why is the sky blue ".repeat(20)).is_ok());

        // A changed provider pauses it; an unknown profile offers nothing.
        let mut selection = crate::ai::SettingsDraft::load(&lib, &profile);
        selection.set_model(Some("other-model".into()));
        let mut lib = lib;
        selection.save(&mut lib, Some(profile.as_str()));
        assert_eq!(offered(&lib, &profile), Some("Ollama"), "the row stays and says why");
        assert!(check(&lib, &profile, "what is rust").unwrap_err().starts_with("The AI provider changed since you turned on Ask AI in the Command Bar."));
        assert_eq!(offered(&lib, "missing-profile"), None);
    }
}
