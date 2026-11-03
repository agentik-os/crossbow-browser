//! Settings → Account's browser sign-in: whether the sync service offers
//! it, the wait for the system browser, the code's redemption at the sync
//! service and a link's confirmation. The protocol is in
//! `crossbow_core::sync::browser_sign_in`; this is its state for the page.

use super::{SyncGlue, device_name, token_store};
use crate::site_permissions::Apps;
use crossbow_core::proto::{ClerkConfig, ClerkExchangeRequest, error_codes};
use crossbow_core::sync::SyncCommand;
use crossbow_core::sync::browser_sign_in::{self as flow, Attempt, BindError, Outcome, Waiting};
use std::sync::{Arc, mpsc};
use std::time::Duration;

/// Whether the sync service offers browser sign-in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Offer {
    Unknown,
    Checking,
    Available(ClerkConfig),
    /// The service answered without a Clerk configuration.
    Unavailable,
    /// The service couldn't be asked.
    Unreachable,
}

/// Where a browser sign-in is. `link` is a signed-in account linking one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Idle,
    /// The system browser is open on the sign-in page; the listener waits.
    Waiting { link: bool },
    /// The browser answered; the sync service is redeeming the code.
    Redeeming { link: bool },
    /// The sync service holds a browser sign-in for this account until the
    /// person confirms it, naming its email.
    Confirm { link_id: String, email: Option<String> },
    Confirming,
}

/// What the last attempt came to. Each says what to do next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    SignedIn,
    Linked,
    NotLinked,
    Cancelled,
    TimedOut,
    Denied,
    LinkRequired,
    IdentityInUse,
    EmailUnverified,
    Network,
    ProviderUnavailable,
    NotConfigured,
    NoBrowser,
    /// Manage Account in Browser couldn't hand its page to the system.
    PortalUnopened,
    PortsBusy,
    Mismatch,
    Expired,
    SessionEnded,
    Failed(String),
}

impl Notice {
    pub fn text(&self) -> String {
        match self {
            Notice::SignedIn => "Signed in with your browser. Sync starts now.".into(),
            Notice::Linked => "Browser sign-in linked. Continue in Browser now signs in to this account.".into(),
            Notice::NotLinked => "Not linked. This account is unchanged.".into(),
            Notice::Cancelled => "Browser sign-in canceled. Try again when you're ready.".into(),
            Notice::TimedOut => "The browser didn't finish within 5 minutes. Try again.".into(),
            Notice::Denied => "Sign-in was canceled in the browser. Try again, or use an email code.".into(),
            Notice::LinkRequired => "An account already uses this email. Sign in with an email code, then choose Link Browser Sign-In… in Account.".into(),
            Notice::IdentityInUse => "This browser sign-in is linked to another Crossbow account. Sign in to that account instead, or try again with another one in the browser.".into(),
            Notice::EmailUnverified => "Your browser sign-in has no verified email address. Verify it in the browser and try again, or use an email code.".into(),
            Notice::Network => "Can't reach the sync service. Check your connection, then try again.".into(),
            Notice::ProviderUnavailable => "The sign-in service didn't answer. Try again in a moment, or use an email code.".into(),
            Notice::NotConfigured => "This sync server doesn't offer browser sign-in. Use an email code.".into(),
            Notice::NoBrowser => "Couldn't open your default browser. Check that one is set, then try again, or use an email code.".into(),
            Notice::PortalUnopened => "Couldn't open your default browser. Check that one is set, then try again.".into(),
            Notice::PortsBusy => "Ports 47231, 47232 and 47233 on this Mac are in use, so the browser can't send the sign-in back. Quit the app using them and try again, or use an email code.".into(),
            Notice::Mismatch => "The browser answered for a different sign-in, so it was ignored. Try again.".into(),
            Notice::Expired => "That browser sign-in expired or was already used. Try again.".into(),
            Notice::SessionEnded => "This Mac's session has ended. Sign out, sign in again, then link.".into(),
            Notice::Failed(why) => format!("Browser sign-in didn't finish ({why}). Try again."),
        }
    }

    /// Shown as a problem, not as a result.
    pub fn is_error(&self) -> bool {
        !matches!(self, Notice::SignedIn | Notice::Linked | Notice::NotLinked | Notice::Cancelled)
    }

    /// Try Again starts a new attempt from here.
    pub fn retry(&self) -> bool {
        !matches!(self, Notice::SignedIn | Notice::Linked | Notice::NotLinked | Notice::LinkRequired | Notice::NotConfigured | Notice::SessionEnded | Notice::PortalUnopened)
    }

    /// The sync service's refusal of a sign-in or link (`"code: message"`).
    fn from_service(error: &str) -> Notice {
        let is = |code: &str| error.starts_with(&format!("{code}:")) || error == code;
        if is(error_codes::LINK_REQUIRED) {
            Notice::LinkRequired
        } else if is(error_codes::IDENTITY_IN_USE) {
            Notice::IdentityInUse
        } else if is(error_codes::EMAIL_UNVERIFIED) {
            Notice::EmailUnverified
        } else if is(error_codes::CLERK_UNAVAILABLE) {
            Notice::NotConfigured
        } else if is(error_codes::PROVIDER_UNAVAILABLE) {
            Notice::ProviderUnavailable
        } else if is(error_codes::SIGN_IN_REJECTED) || is(error_codes::NOT_FOUND) {
            Notice::Expired
        } else if is(error_codes::UNAUTHORIZED) {
            Notice::SessionEnded
        } else if error.starts_with("network:") {
            Notice::Network
        } else {
            Notice::Failed(error.chars().take(120).collect())
        }
    }
}

enum Message {
    Offer { base_url: String, result: Result<Option<ClerkConfig>, String> },
    Outcome { attempt: u64, outcome: Outcome },
}

pub struct BrowserSignIn {
    pub offer: Offer,
    pub step: Step,
    pub notice: Option<Notice>,
    /// A browser sign-in is linked to the signed-in account; `None` until
    /// the sync service says.
    pub linked: Option<bool>,
    /// The system's apps: the default browser opens the sign-in page.
    pub(crate) apps: Box<dyn Apps>,
    /// How long the browser has; shorter only in tests.
    pub(crate) timeout: Duration,
    offer_for: String,
    attempt: u64,
    last_link: bool,
    secrets: Option<(Attempt, String)>,
    waiting: Option<Waiting>,
    tx: mpsc::Sender<Message>,
    rx: mpsc::Receiver<Message>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl BrowserSignIn {
    pub(super) fn new(wake: Arc<dyn Fn() + Send + Sync>) -> BrowserSignIn {
        let (tx, rx) = mpsc::channel();
        #[cfg(not(test))]
        let apps: Box<dyn Apps> = Box::new(crate::site_permissions::Native);
        // Tests never open the system browser.
        #[cfg(test)]
        let apps: Box<dyn Apps> = Box::new(tests::NoApps);
        BrowserSignIn {
            offer: Offer::Unknown, step: Step::Idle, notice: None, linked: None, apps, timeout: flow::TIMEOUT,
            offer_for: String::new(), attempt: 0, last_link: false, secrets: None, waiting: None, tx, rx, wake,
        }
    }

    pub fn busy(&self) -> bool {
        self.step != Step::Idle
    }

    /// End any attempt: the listener closes and late answers are ignored.
    fn end(&mut self) {
        self.attempt += 1;
        self.waiting = None;
        self.secrets = None;
        self.step = Step::Idle;
    }

    /// Signed out or another account: nothing carries over.
    pub(super) fn reset(&mut self) {
        self.end();
        self.notice = None;
        self.linked = None;
    }

    pub fn available(&self) -> Option<&ClerkConfig> {
        match &self.offer {
            Offer::Available(config) => Some(config),
            _ => None,
        }
    }
}

/// Hand `url` to the system as any link, the same way whichever browser is
/// the default. When that is Crossbow, the page comes back as a link from
/// another app and opens as an ordinary tab under the Links rules; nothing
/// here reads, scripts or watches it. Only the loopback listener hears back.
fn open_in_browser(apps: &mut dyn Apps, url: &str) -> bool {
    apps.handler(url).is_some() && apps.open(url)
}

impl SyncGlue {
    /// Ask the sync service whether it offers browser sign-in, once per
    /// server, or again after Try Again.
    pub fn check_browser_sign_in(&mut self) {
        let b = &mut self.browser;
        if b.offer_for == self.base_url && b.offer != Offer::Unknown {
            return;
        }
        b.offer_for = self.base_url.clone();
        b.offer = Offer::Checking;
        let (tx, wake, base_url) = (b.tx.clone(), b.wake.clone(), self.base_url.clone());
        flow::check_server(base_url.clone(), move |result| {
            let _ = tx.send(Message::Offer { base_url, result });
            wake();
        });
    }

    pub fn recheck_browser_sign_in(&mut self) {
        self.browser.offer = Offer::Unknown;
        self.check_browser_sign_in();
    }

    /// Continue in Browser (signed out) or Link Browser Sign-In… (signed in).
    pub fn start_browser_sign_in(&mut self) {
        let link = self.signed_in();
        if self.browser.busy() || !link && self.busy { return; }
        let Some(config) = self.browser.available().cloned() else { return };
        self.browser.end();
        self.browser.notice = None;
        self.browser.last_link = link;
        if let Err(notice) = self.open_attempt(&config, link) {
            self.browser.end();
            self.browser.notice = Some(notice);
        }
    }

    fn open_attempt(&mut self, config: &ClerkConfig, link: bool) -> Result<(), Notice> {
        let secrets = Attempt::new().map_err(Notice::Failed)?;
        let listener = flow::bind().map_err(|e| match e {
            BindError::Busy => Notice::PortsBusy,
            BindError::Failed(e) => Notice::Failed(e),
        })?;
        let redirect_uri = listener.redirect_uri().to_string();
        let url = flow::authorize_url(config, &redirect_uri, &secrets).map_err(Notice::Failed)?;
        let b = &mut self.browser;
        let (tx, wake, attempt) = (b.tx.clone(), b.wake.clone(), b.attempt);
        // Listening before the browser opens, so its answer always lands.
        let waiting = listener.wait(secrets.state.clone(), b.timeout, move |outcome| {
            let _ = tx.send(Message::Outcome { attempt, outcome });
            wake();
        }).map_err(Notice::Failed)?;
        b.waiting = Some(waiting);
        b.secrets = Some((secrets, redirect_uri));
        if !open_in_browser(&mut *b.apps, &url) { return Err(Notice::NoBrowser) }
        b.step = Step::Waiting { link };
        Ok(())
    }

    /// Try Again: a new attempt with new secrets, nothing kept from the last.
    pub fn retry_browser_sign_in(&mut self) {
        if self.browser.last_link != self.signed_in() { return; }
        self.browser.notice = None;
        self.start_browser_sign_in();
    }

    /// Cancel, or Escape: closes the listener at once. False when nothing
    /// was in progress.
    pub fn cancel_browser_sign_in(&mut self) -> bool {
        let notice = match &self.browser.step {
            Step::Idle => return false,
            Step::Confirm { .. } | Step::Confirming => Notice::NotLinked,
            Step::Redeeming { link: false } => {
                // A session the service issues after this is revoked unused.
                self.handle.send(SyncCommand::CancelAuth);
                self.busy = false;
                Notice::Cancelled
            }
            Step::Waiting { .. } | Step::Redeeming { link: true } => Notice::Cancelled,
        };
        self.browser.end();
        self.browser.notice = Some(notice);
        true
    }

    /// The Clerk Account Portal page Manage Account in Browser opens: only
    /// while signed in to an account with a browser sign-in linked, however
    /// this session signed in, on a server whose health names a Clerk
    /// configuration the portal can be derived from.
    pub fn account_portal(&self) -> Option<String> {
        if !self.signed_in() || self.browser.linked != Some(true) { return None }
        flow::account_portal_url(self.browser.available()?)
    }

    /// Manage Account in Browser: hand the portal page to the system, as
    /// browser sign-in does. Crossbow keeps nothing from it.
    pub fn manage_account(&mut self) {
        let Some(url) = self.account_portal() else { return };
        self.browser.notice = (!open_in_browser(&mut *self.browser.apps, &url)).then_some(Notice::PortalUnopened);
    }

    /// Link, in the confirmation naming both emails.
    pub fn confirm_browser_link(&mut self) {
        let Step::Confirm { link_id, .. } = &self.browser.step else { return };
        let link_id = link_id.clone();
        let Some(token) = self.session_token() else { return self.stop_browser(Notice::SessionEnded) };
        let base_url = self.session.as_ref().map(|s| s.base_url.clone()).unwrap_or_default();
        self.handle.send(SyncCommand::ConfirmLink { attempt: self.browser.attempt, base_url, token, link_id });
        self.browser.step = Step::Confirming;
    }

    pub(super) fn session_token(&self) -> Option<String> {
        let s = self.session.as_ref()?;
        token_store::load(&self.data_dir, &s.account_id).ok().flatten()
    }

    /// The browser's answers and the server check. Called from `poll`.
    pub(super) fn poll_browser(&mut self) {
        while let Ok(message) = self.browser.rx.try_recv() {
            match message {
                Message::Offer { base_url, result } if base_url == self.browser.offer_for => {
                    self.browser.offer = match result {
                        Ok(Some(config)) => Offer::Available(config),
                        Ok(None) => Offer::Unavailable,
                        Err(_) => Offer::Unreachable,
                    };
                }
                Message::Offer { .. } => {}
                Message::Outcome { attempt, outcome } if attempt == self.browser.attempt => self.answered(outcome),
                Message::Outcome { .. } => {}
            }
        }
    }

    fn answered(&mut self, outcome: Outcome) {
        let Step::Waiting { link } = self.browser.step else { return };
        // The listener has closed; its thread ends on its own.
        self.browser.waiting = None;
        let code = match outcome {
            Outcome::Code(code) => code,
            Outcome::Denied => return self.stop_browser(Notice::Denied),
            Outcome::TimedOut => return self.stop_browser(Notice::TimedOut),
            Outcome::StateMismatch => return self.stop_browser(Notice::Mismatch),
            Outcome::Cancelled => return self.stop_browser(Notice::Cancelled),
            Outcome::Failed(why) => return self.stop_browser(Notice::Failed(why)),
        };
        let Some((secrets, redirect_uri)) = self.browser.secrets.take() else { return };
        let exchange = ClerkExchangeRequest {
            code,
            code_verifier: secrets.verifier,
            redirect_uri,
            nonce: secrets.nonce,
            device_name: device_name(),
            platform: crossbow_core::platform().to_string(),
        };
        if link {
            let Some(token) = self.session_token() else { return self.stop_browser(Notice::SessionEnded) };
            let base_url = self.session.as_ref().map(|s| s.base_url.clone()).unwrap_or_default();
            self.handle.send(SyncCommand::LinkBrowserSignIn { attempt: self.browser.attempt, base_url, token, exchange });
        } else {
            self.error = None;
            self.busy = true;
            self.handle.send(SyncCommand::BrowserSignIn { base_url: self.base_url.clone(), exchange });
        }
        self.browser.step = Step::Redeeming { link };
    }

    fn stop_browser(&mut self, notice: Notice) {
        self.browser.end();
        self.browser.notice = Some(notice);
    }

    /// A sign-in's refusal, if it was this page's browser sign-in.
    pub(super) fn browser_sign_in_failed(&mut self, error: &str) -> bool {
        if self.browser.step != (Step::Redeeming { link: false }) { return false; }
        self.stop_browser(Notice::from_service(error));
        true
    }

    /// A session arrived: ends this page's browser sign-in, if it was one.
    pub(super) fn take_browser_sign_in(&mut self) -> bool {
        let redeeming = self.browser.step == (Step::Redeeming { link: false });
        if redeeming { self.browser.end(); }
        redeeming
    }

    pub(super) fn link_pending(&mut self, attempt: u64, link_id: Option<String>, email: Option<String>) {
        if attempt != self.browser.attempt || self.browser.step != (Step::Redeeming { link: true }) { return; }
        match link_id {
            Some(link_id) => self.browser.step = Step::Confirm { link_id, email },
            // This sign-in is this account's already.
            None => {
                self.browser.end();
                self.browser.linked = Some(true);
                self.browser.notice = Some(Notice::Linked);
            }
        }
    }

    pub(super) fn linked(&mut self, attempt: u64) {
        if attempt != self.browser.attempt || self.browser.step != Step::Confirming { return; }
        self.browser.end();
        self.browser.linked = Some(true);
        self.browser.notice = Some(Notice::Linked);
    }

    pub(super) fn link_failed(&mut self, attempt: u64, error: &str) {
        if attempt != self.browser.attempt { return; }
        self.stop_browser(Notice::from_service(error));
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use crate::site_permissions::{Apps, Handler};

    /// No browser and no system: the default in tests.
    pub(crate) struct NoApps;

    impl Apps for NoApps {
        fn handler(&self, _url: &str) -> Option<Handler> { None }
        fn open(&mut self, _url: &str) -> bool { false }
        fn own_bundle(&self) -> Option<String> { None }
        fn declares(&self, _key: &str) -> bool { true }
    }
}
