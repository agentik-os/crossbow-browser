//! The preview fetch against fake CEF request, response and URL request
//! objects: what is asked for, when the body is read, and each outcome.

use super::*;
use cef::rc::{ConvertReturnValue, RcImpl};

fn userfree(text: &str) -> sys::cef_string_userfree_t {
    let text: Vec<u16> = text.encode_utf16().collect();
    unsafe {
        let raw = sys::cef_string_userfree_utf16_alloc();
        if !raw.is_null() && sys::cef_string_utf16_set(text.as_ptr(), text.len(), raw, 1) == 0 {
            sys::cef_string_userfree_utf16_free(raw);
            return std::ptr::null_mut();
        }
        raw
    }
}

unsafe fn text(raw: *const sys::cef_string_t) -> String {
    let Some(text) = (unsafe { raw.as_ref() }) else { return String::new() };
    if text.length == 0 || text.str_.is_null() { return String::new(); }
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text.str_, text.length) })
}

// -- The request ------------------------------------------------------------------

#[derive(Default)]
struct Asked {
    url: String,
    method: String,
    flags: Option<i32>,
    referrer: Option<(String, sys::cef_referrer_policy_t)>,
    headers: Vec<(String, String, i32)>,
}

type AskedSpy = Rc<RefCell<Asked>>;

fn asked(raw: *mut sys::_cef_request_t) -> std::cell::RefMut<'static, Asked> {
    RcImpl::<sys::_cef_request_t, AskedSpy>::get(raw).interface.borrow_mut()
}

unsafe extern "C" fn set_url(raw: *mut sys::_cef_request_t, url: *const sys::cef_string_t) { asked(raw).url = unsafe { text(url) }; }
unsafe extern "C" fn set_method(raw: *mut sys::_cef_request_t, method: *const sys::cef_string_t) { asked(raw).method = unsafe { text(method) }; }
unsafe extern "C" fn set_flags(raw: *mut sys::_cef_request_t, flags: i32) { asked(raw).flags = Some(flags); }
unsafe extern "C" fn set_referrer(raw: *mut sys::_cef_request_t, url: *const sys::cef_string_t, policy: sys::cef_referrer_policy_t) {
    asked(raw).referrer = Some((unsafe { text(url) }, policy));
}
unsafe extern "C" fn set_header(raw: *mut sys::_cef_request_t, name: *const sys::cef_string_t, value: *const sys::cef_string_t, overwrite: i32) {
    asked(raw).headers.push((unsafe { text(name) }, unsafe { text(value) }, overwrite));
}

#[test]
fn the_request_is_a_get_without_credentials_cache_referer_or_redirects() {
    crate::load_cef_strings();
    let spy: AskedSpy = Rc::default();
    let mut raw: sys::_cef_request_t = unsafe { std::mem::zeroed() };
    raw.set_url = Some(set_url);
    raw.set_method = Some(set_method);
    raw.set_flags = Some(set_flags);
    raw.set_referrer = Some(set_referrer);
    raw.set_header_by_name = Some(set_header);
    let request: Request = RcImpl::new(raw, spy.clone()).cast::<sys::_cef_request_t>().wrap_result();
    prepare(&request, "https://example.test/article");
    let asked = spy.borrow();
    assert_eq!((asked.url.as_str(), asked.method.as_str()), ("https://example.test/article", "GET"));
    let flags = asked.flags.expect("flags are set") as u32;
    assert_ne!(flags & sys::cef_urlrequest_flags_t::UR_FLAG_STOP_ON_REDIRECT.0, 0, "redirects stop for the caller to check");
    assert_ne!(flags & sys::cef_urlrequest_flags_t::UR_FLAG_DISABLE_CACHE.0, 0, "nothing is cached");
    assert_eq!(flags & sys::cef_urlrequest_flags_t::UR_FLAG_ALLOW_STORED_CREDENTIALS.0, 0, "no cookies or HTTP auth");
    assert_eq!(flags, FLAGS, "and nothing else");
    assert_eq!(asked.referrer, Some((String::new(), sys::cef_referrer_policy_t::REFERRER_POLICY_NO_REFERRER)));
    assert_eq!(asked.headers, [("Accept".to_owned(), ACCEPT.to_owned(), 1)]);
}

// -- The response and the URL request -------------------------------------------

struct ResponseSpy(Head);

fn head(raw: *mut sys::_cef_response_t) -> &'static Head {
    &RcImpl::<sys::_cef_response_t, ResponseSpy>::get(raw).interface.0
}

unsafe extern "C" fn status(raw: *mut sys::_cef_response_t) -> i32 { i32::from(head(raw).status) }
unsafe extern "C" fn mime(raw: *mut sys::_cef_response_t) -> sys::cef_string_userfree_t { userfree(&head(raw).mime) }
unsafe extern "C" fn charset(raw: *mut sys::_cef_response_t) -> sys::cef_string_userfree_t { userfree(&head(raw).charset) }
unsafe extern "C" fn url(raw: *mut sys::_cef_response_t) -> sys::cef_string_userfree_t { userfree(&head(raw).url) }
unsafe extern "C" fn header(raw: *mut sys::_cef_response_t, name: *const sys::cef_string_t) -> sys::cef_string_userfree_t {
    let head = head(raw);
    match unsafe { text(name) }.to_ascii_lowercase().as_str() {
        "content-disposition" => userfree(&head.disposition),
        "location" => userfree(&head.location),
        _ => userfree(""),
    }
}

fn response(head: &Head) -> *mut sys::_cef_response_t {
    let mut raw: sys::_cef_response_t = unsafe { std::mem::zeroed() };
    raw.get_status = Some(status);
    raw.get_mime_type = Some(mime);
    raw.get_charset = Some(charset);
    raw.get_url = Some(url);
    raw.get_header_by_name = Some(header);
    RcImpl::new(raw, ResponseSpy(head.clone())).cast::<sys::_cef_response_t>()
}

struct Exchange {
    head: Option<Head>,
    status: sys::cef_urlrequest_status_t,
    error: sys::cef_errorcode_t,
    cancels: usize,
}

type ExchangeSpy = Rc<RefCell<Exchange>>;

fn exchange(raw: *mut sys::_cef_urlrequest_t) -> &'static ExchangeSpy {
    &RcImpl::<sys::_cef_urlrequest_t, ExchangeSpy>::get(raw).interface
}

unsafe extern "C" fn get_response(raw: *mut sys::_cef_urlrequest_t) -> *mut sys::_cef_response_t {
    exchange(raw).borrow().head.as_ref().map_or(std::ptr::null_mut(), response)
}
unsafe extern "C" fn get_status(raw: *mut sys::_cef_urlrequest_t) -> sys::cef_urlrequest_status_t { exchange(raw).borrow().status }
unsafe extern "C" fn get_error(raw: *mut sys::_cef_urlrequest_t) -> sys::cef_errorcode_t { exchange(raw).borrow().error }
unsafe extern "C" fn cancel(raw: *mut sys::_cef_urlrequest_t) {
    let spy = exchange(raw);
    let mut exchange = spy.borrow_mut();
    exchange.cancels += 1;
    exchange.status = sys::cef_urlrequest_status_t::UR_CANCELED;
    exchange.error = sys::cef_errorcode_t::ERR_ABORTED;
}

/// A URL request whose response has `head`, with the client that reads it.
fn fake(head: Option<Head>) -> (Urlrequest, UrlrequestClient, Rc<RefCell<State>>, ExchangeSpy) {
    crate::load_cef_strings();
    let spy = Rc::new(RefCell::new(Exchange { head, status: sys::cef_urlrequest_status_t::UR_IO_PENDING, error: sys::cef_errorcode_t::ERR_NONE, cancels: 0 }));
    let mut raw: sys::_cef_urlrequest_t = unsafe { std::mem::zeroed() };
    raw.get_response = Some(get_response);
    raw.get_request_status = Some(get_status);
    raw.get_request_error = Some(get_error);
    raw.cancel = Some(cancel);
    let request = RcImpl::new(raw, spy.clone()).cast::<sys::_cef_urlrequest_t>().wrap_result();
    let state = Rc::new(RefCell::new(State::default()));
    (request, PreviewClient::new(state.clone()), state, spy)
}

fn html() -> Head {
    Head { status: 200, mime: "text/html".into(), charset: "utf-8".into(), url: "https://example.test/article".into(), ..Head::default() }
}

fn send(client: &UrlrequestClient, request: &mut Urlrequest, data: &[u8]) {
    client.on_download_progress(Some(request), 0, -1);
    client.on_download_data(Some(request), data.as_ptr(), data.len());
}

fn finish(client: &UrlrequestClient, request: &mut Urlrequest, spy: &ExchangeSpy, status: sys::cef_urlrequest_status_t, error: sys::cef_errorcode_t) {
    {
        let mut exchange = spy.borrow_mut();
        if exchange.status != sys::cef_urlrequest_status_t::UR_CANCELED { exchange.status = status; exchange.error = error; }
    }
    client.on_request_complete(Some(request));
}

fn outcome(state: &Rc<RefCell<State>>) -> FetchOutcome {
    state.borrow_mut().outcome.take().expect("the request ended")
}

#[test]
fn an_html_page_is_read_whole_without_cancelling() {
    let (mut request, client, state, spy) = fake(Some(html()));
    send(&client, &mut request, b"<title>Fixture</title>");
    send(&client, &mut request, b"<p>Body</p>");
    finish(&client, &mut request, &spy, sys::cef_urlrequest_status_t::UR_SUCCESS, sys::cef_errorcode_t::ERR_NONE);
    assert_eq!(spy.borrow().cancels, 0);
    assert_eq!(outcome(&state), FetchOutcome::Page { url: "https://example.test/article".into(), charset: "utf-8".into(), body: b"<title>Fixture</title><p>Body</p>".to_vec(), truncated: false });
}

#[test]
fn the_body_stops_at_its_cap_and_the_request_is_cancelled() {
    let (mut request, client, state, spy) = fake(Some(html()));
    let chunk = vec![b'a'; 300 * 1024];
    for _ in 0..4 { send(&client, &mut request, &chunk); }
    assert_eq!(spy.borrow().cancels, 1, "cancelled once the cap is reached");
    send(&client, &mut request, &chunk);
    assert!(spy.borrow().cancels >= 2, "a late chunk is refused too");
    finish(&client, &mut request, &spy, sys::cef_urlrequest_status_t::UR_SUCCESS, sys::cef_errorcode_t::ERR_NONE);
    let FetchOutcome::Page { body, truncated, .. } = outcome(&state) else { panic!("a page") };
    assert_eq!(body.len(), BODY_BYTES);
    assert!(truncated);
}

#[test]
fn other_responses_stop_at_the_headers_and_read_no_body() {
    for (case, head, expected) in [
        ("pdf", Head { mime: "application/pdf".into(), ..html() }, FetchOutcome::NotHtml { mime: "application/pdf".into(), attachment: false }),
        ("attachment", Head { disposition: "attachment; filename=\"fixture.html\"".into(), ..html() }, FetchOutcome::NotHtml { mime: "text/html".into(), attachment: true }),
        ("missing", Head { status: 404, ..html() }, FetchOutcome::Status(404)),
        ("server", Head { status: 503, ..html() }, FetchOutcome::Status(503)),
    ] {
        let (mut request, client, state, spy) = fake(Some(head));
        send(&client, &mut request, b"%PDF-1.7 fixture bytes");
        assert!(spy.borrow().cancels >= 1, "{case}: stopped at the headers");
        assert!(state.borrow().body.is_empty(), "{case}: no body was kept");
        finish(&client, &mut request, &spy, sys::cef_urlrequest_status_t::UR_SUCCESS, sys::cef_errorcode_t::ERR_NONE);
        assert_eq!(outcome(&state), expected, "{case}");
    }
}

#[test]
fn a_redirect_ends_the_request_with_its_location_for_the_caller_to_check() {
    for status in [301, 302, 303, 307, 308] {
        let (mut request, client, state, spy) = fake(Some(Head { status, location: "/next".into(), ..html() }));
        // Stopped by UR_FLAG_STOP_ON_REDIRECT: no data, then the end.
        finish(&client, &mut request, &spy, sys::cef_urlrequest_status_t::UR_CANCELED, sys::cef_errorcode_t::ERR_ABORTED);
        assert_eq!(outcome(&state), FetchOutcome::Redirect { status, location: "/next".into() });
    }
    let (mut request, client, state, spy) = fake(Some(Head { status: 302, ..html() }));
    finish(&client, &mut request, &spy, sys::cef_urlrequest_status_t::UR_FAILED, sys::cef_errorcode_t::ERR_ABORTED);
    assert_eq!(outcome(&state), FetchOutcome::Failed(-3), "a redirect without a Location is a failure");
}

#[test]
fn network_failures_keep_their_error_code() {
    for (error, code) in [(sys::cef_errorcode_t::ERR_INTERNET_DISCONNECTED, -106), (sys::cef_errorcode_t::ERR_NAME_NOT_RESOLVED, -105), (sys::cef_errorcode_t::ERR_CONNECTION_REFUSED, -102)] {
        let (mut request, client, state, spy) = fake(None);
        finish(&client, &mut request, &spy, sys::cef_urlrequest_status_t::UR_FAILED, error);
        assert_eq!(outcome(&state), FetchOutcome::Failed(code));
    }
    // An empty HTML page is still a page.
    let (mut request, client, state, spy) = fake(Some(html()));
    finish(&client, &mut request, &spy, sys::cef_urlrequest_status_t::UR_SUCCESS, sys::cef_errorcode_t::ERR_NONE);
    assert!(matches!(outcome(&state), FetchOutcome::Page { body, .. } if body.is_empty()));
}

#[test]
fn dropping_an_unfinished_fetch_cancels_it_and_a_finished_one_is_left_alone() {
    let (request, _client, state, spy) = fake(Some(html()));
    drop(PageFetch { request, state });
    assert_eq!(spy.borrow().cancels, 1);

    let (mut request, client, state, spy) = fake(Some(html()));
    finish(&client, &mut request, &spy, sys::cef_urlrequest_status_t::UR_SUCCESS, sys::cef_errorcode_t::ERR_NONE);
    let fetch = PageFetch { request, state };
    assert!(fetch.poll().is_some());
    assert!(fetch.poll().is_none(), "an outcome is given once");
    drop(fetch);
    assert_eq!(spy.borrow().cancels, 0);
}
