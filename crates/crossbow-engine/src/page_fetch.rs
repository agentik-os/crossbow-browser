//! One page fetched for a link preview: a GET in the linking view's own
//! profile context that sends and saves no cookies or HTTP auth, skips the
//! cache, sends no Referer and stops at a redirect, so the caller checks each
//! hop. The headers decide before any body is read: only an HTML response
//! that isn't an attachment is read, up to `BODY_BYTES`. Nothing is written
//! to disk and no download starts.

use cef::*;
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

/// The most of a page's body that is read.
pub const BODY_BYTES: usize = 1 << 20;

/// No stored credentials, no cache and no automatic redirects.
pub(crate) const FLAGS: u32 = sys::cef_urlrequest_flags_t::UR_FLAG_DISABLE_CACHE.0 | sys::cef_urlrequest_flags_t::UR_FLAG_STOP_ON_REDIRECT.0;
pub(crate) const ACCEPT: &str = "text/html,application/xhtml+xml";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FetchOutcome {
    /// An HTML page: its address, the charset its headers name, and its body,
    /// cut at `BODY_BYTES` when `truncated`.
    Page { url: String, charset: String, body: Vec<u8>, truncated: bool },
    /// A redirect, not followed: its status and its Location as sent.
    Redirect { status: u16, location: String },
    /// Not an HTML page, or an attachment: only the headers were read.
    NotHtml { mime: String, attachment: bool },
    /// An HTTP status other than success, read from the headers.
    Status(u16),
    /// The request failed with this network error, such as -106 offline.
    Failed(i32),
}

/// What the headers say, read once.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Head {
    pub(crate) status: u16,
    pub(crate) mime: String,
    pub(crate) charset: String,
    pub(crate) url: String,
    pub(crate) disposition: String,
    pub(crate) location: String,
}

impl Head {
    fn read(response: &Response) -> Head {
        let header = |name: &str| CefString::from(&response.header_by_name(Some(&CefString::from(name)))).to_string();
        Head {
            status: u16::try_from(response.status()).unwrap_or(0),
            mime: CefString::from(&response.mime_type()).to_string().to_ascii_lowercase(),
            charset: CefString::from(&response.charset()).to_string(),
            url: CefString::from(&response.url()).to_string(),
            disposition: header("Content-Disposition"),
            location: header("Location"),
        }
    }

    fn redirect(&self) -> bool {
        matches!(self.status, 301 | 302 | 303 | 307 | 308) && !self.location.is_empty()
    }

    /// Why the body isn't read, if it isn't.
    fn refusal(&self) -> Option<FetchOutcome> {
        if self.redirect() { return Some(FetchOutcome::Redirect { status: self.status, location: self.location.clone() }); }
        if !(200..300).contains(&self.status) { return Some(FetchOutcome::Status(self.status)); }
        let attachment = self.disposition.trim_start().to_ascii_lowercase().starts_with("attachment");
        if attachment || !matches!(self.mime.as_str(), "text/html" | "application/xhtml+xml") {
            return Some(FetchOutcome::NotHtml { mime: self.mime.clone(), attachment });
        }
        None
    }
}

#[derive(Debug, Default)]
pub(crate) struct State {
    head: Option<Head>,
    /// Decided at the headers or at the size cap; the request was cancelled.
    stopped: Option<FetchOutcome>,
    body: Vec<u8>,
    truncated: bool,
    ended: bool,
    outcome: Option<FetchOutcome>,
}

impl State {
    /// Reads the headers the first time. True while the body is wanted.
    fn wants_body(&mut self, response: Option<Response>) -> bool {
        if self.stopped.is_some() || self.ended { return false; }
        if self.head.is_none() {
            let Some(response) = response else { return false };
            let head = Head::read(&response);
            self.stopped = head.refusal();
            self.head = Some(head);
        }
        self.stopped.is_none()
    }

    /// Keeps up to `BODY_BYTES`. False once the cap is reached.
    fn take(&mut self, data: &[u8]) -> bool {
        let room = BODY_BYTES.saturating_sub(self.body.len());
        self.body.extend_from_slice(&data[..data.len().min(room)]);
        if data.len() >= room {
            self.truncated = data.len() > room || self.truncated;
            self.stopped = Some(self.page());
            return false;
        }
        true
    }

    fn page(&mut self) -> FetchOutcome {
        let head = self.head.clone().unwrap_or_default();
        FetchOutcome::Page { url: head.url, charset: head.charset, body: std::mem::take(&mut self.body), truncated: self.truncated }
    }

    fn complete(&mut self, status: UrlrequestStatus, error: i32, response: Option<Response>) {
        if self.ended { return; }
        self.ended = true;
        if self.head.is_none() && let Some(response) = response {
            let head = Head::read(&response);
            // A redirect stopped by the flag ends as a failure with its headers.
            if head.redirect() || status == UrlrequestStatus::SUCCESS { self.stopped = self.stopped.take().or_else(|| head.refusal()); }
            self.head = Some(head);
        }
        self.outcome = Some(match self.stopped.take() {
            Some(outcome) => outcome,
            None if status == UrlrequestStatus::SUCCESS => self.page(),
            None => FetchOutcome::Failed(if error == 0 { -2 } else { error }),
        });
    }
}

/// A fetch in flight. Dropping it cancels it.
pub struct PageFetch {
    request: Urlrequest,
    state: Rc<RefCell<State>>,
}

impl PageFetch {
    /// The outcome, once, when the request has ended.
    pub fn poll(&self) -> Option<FetchOutcome> {
        self.state.borrow_mut().outcome.take()
    }

    pub fn cancel(&self) {
        let open = !self.state.borrow().ended;
        if open { self.request.cancel(); }
    }
}

impl Drop for PageFetch {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Starts fetching `url` in `profile_id`'s own request context. Call on the
/// browser process UI thread; the outcome arrives there and wakes the UI.
pub fn fetch_page(data_dir: &Path, profile_id: &str, url: &str) -> Result<PageFetch, &'static str> {
    let mut context = crate::profile_context(data_dir, profile_id).ok_or("This profile's website data isn't available.")?;
    let mut request = request_create().ok_or("The preview request couldn't start.")?;
    prepare(&request, url);
    let state = Rc::new(RefCell::new(State::default()));
    let mut client = PreviewClient::new(state.clone());
    let request = urlrequest_create(Some(&mut request), Some(&mut client), Some(&mut context)).ok_or("The preview request couldn't start.")?;
    Ok(PageFetch { request, state })
}

pub(crate) fn prepare(request: &Request, url: &str) {
    request.set_url(Some(&CefString::from(url)));
    request.set_method(Some(&CefString::from("GET")));
    request.set_flags(FLAGS as std::os::raw::c_int);
    request.set_referrer(Some(&CefString::from("")), ReferrerPolicy::NO_REFERRER);
    request.set_header_by_name(Some(&CefString::from("Accept")), Some(&CefString::from(ACCEPT)), 1);
}

wrap_urlrequest_client! {
    pub(crate) struct PreviewClient {
        state: Rc<RefCell<State>>,
    }

    impl UrlrequestClient {
        fn on_download_progress(&self, request: Option<&mut Urlrequest>, _current: i64, _total: i64) {
            let Some(request) = request else { return };
            let wanted = self.state.borrow_mut().wants_body(request.response());
            if !wanted { request.cancel(); }
        }

        fn on_download_data(&self, request: Option<&mut Urlrequest>, data: *const u8, data_length: usize) {
            let Some(request) = request else { return };
            let mut keep = self.state.borrow_mut().wants_body(request.response());
            if keep && !data.is_null() && data_length > 0 {
                let data = unsafe { std::slice::from_raw_parts(data, data_length) };
                keep = self.state.borrow_mut().take(data);
            }
            // Cancelling may complete the request at once, so no borrow is held.
            if !keep { request.cancel(); }
        }

        fn on_request_complete(&self, request: Option<&mut Urlrequest>) {
            let Some(request) = request else { return };
            let (status, error, response) = (request.request_status(), request.request_error().get_raw(), request.response());
            self.state.borrow_mut().complete(status, error, response);
            crate::wake(std::time::Duration::ZERO);
        }

        fn auth_credentials(&self, _is_proxy: ::std::os::raw::c_int, _host: Option<&CefString>, _port: ::std::os::raw::c_int, _realm: Option<&CefString>, _scheme: Option<&CefString>, _callback: Option<&mut AuthCallback>) -> ::std::os::raw::c_int {
            // Never answer a login prompt for a preview.
            0
        }
    }
}

#[cfg(test)]
#[path = "page_fetch_tests.rs"]
mod tests;
