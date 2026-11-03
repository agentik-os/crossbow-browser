//! Command-bar suggestions from the profile's search engine. A request waits
//! for typing to pause, only one runs at a time, and a reply counts only while
//! the bar still wants exactly that request. Failures leave the ordinary
//! results and are logged once per host, naming only the host.

use std::collections::HashSet;
use std::sync::Arc;

/// Typing must pause this long, in seconds, before a request is sent.
pub const DEBOUNCE: f64 = 0.15;

/// One suggestion request. The URL holds the typed text and is never logged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub profile: String,
    pub url: String,
}

impl Request {
    fn host(&self) -> String {
        crossbow_core::search::host_of(&self.url).unwrap_or_else(|| "an unknown host".into())
    }
}

pub trait Transport {
    fn send(&mut self, seq: u64, url: &str);
    /// A finished request: its number, and its suggestions or None on failure.
    fn recv(&mut self) -> Option<(u64, Option<Vec<String>>)>;
}

pub struct Suggestions {
    transport: Box<dyn Transport>,
    /// What the open bar wants, and when it started wanting it.
    wanted: Option<(Request, f64)>,
    /// Sent for the current want already; a failure is not retried.
    attempted: bool,
    in_flight: Option<(u64, Request)>,
    shown: Option<(Request, Vec<String>)>,
    seq: u64,
    revision: u64,
    logged: HashSet<String>,
}

impl Suggestions {
    pub fn new(transport: Box<dyn Transport>) -> Self {
        Suggestions { transport, wanted: None, attempted: false, in_flight: None, shown: None, seq: 0, revision: 0, logged: HashSet::new() }
    }

    /// Requests go out over the network from a worker started on first use.
    pub fn network(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self::new(Box::new(Network { wake: Arc::new(wake), worker: None, failed: Vec::new() }))
    }

    /// What the bar wants now: None when it is closed or nothing may be sent.
    pub fn want(&mut self, now: f64, request: Option<Request>) {
        if self.wanted.as_ref().map(|(wanted, _)| wanted) == request.as_ref() { return; }
        if self.shown.take().is_some() { self.revision += 1; }
        self.wanted = request.map(|request| (request, now));
        self.attempted = false;
    }

    /// Take finished requests; only a reply to the current want is shown.
    pub fn receive(&mut self) {
        while let Some((seq, result)) = self.transport.recv() {
            let Some((_, request)) = self.in_flight.take_if(|(sent, _)| *sent == seq) else { continue };
            // Typing back to text already in flight needs no second request.
            let current = self.wanted.as_ref().is_some_and(|(wanted, _)| *wanted == request);
            self.attempted |= current;
            match result {
                Some(found) if current => {
                    self.shown = Some((request, found));
                    self.revision += 1;
                }
                Some(_) => {}
                None => {
                    let host = request.host();
                    if !self.logged.contains(&host) {
                        log::info!("Search suggestions are unavailable from {host}");
                        self.logged.insert(host);
                    }
                }
            }
        }
    }

    /// Send the wanted request once typing has paused and nothing is in
    /// flight. Returns the seconds until it is due, to schedule a repaint.
    pub fn send_due(&mut self, now: f64) -> Option<f64> {
        let (request, since) = self.wanted.as_ref()?;
        if self.attempted || self.in_flight.is_some() { return None; }
        let due = since + DEBOUNCE;
        if now < due { return Some(due - now); }
        self.seq += 1;
        self.transport.send(self.seq, &request.url);
        self.in_flight = Some((self.seq, request.clone()));
        self.attempted = true;
        None
    }

    /// Suggestions answering exactly `request`.
    pub fn shown(&self, request: &Request) -> &[String] {
        match &self.shown {
            Some((shown, found)) if shown == request => found,
            _ => &[],
        }
    }

    /// Changes whenever shown suggestions appear or go away.
    pub fn revision(&self) -> u64 {
        self.revision
    }
}

struct Network {
    wake: Arc<dyn Fn() + Send + Sync>,
    worker: Option<crossbow_core::suggest::Worker>,
    failed: Vec<u64>,
}

impl Transport for Network {
    fn send(&mut self, seq: u64, url: &str) {
        if self.worker.is_none() {
            let wake = self.wake.clone();
            self.worker = crossbow_core::suggest::Worker::spawn(move || wake()).ok();
        }
        match &self.worker {
            Some(worker) => worker.send(seq, url.to_string()),
            None => self.failed.push(seq),
        }
    }

    fn recv(&mut self) -> Option<(u64, Option<Vec<String>>)> {
        if let Some(seq) = self.failed.pop() { return Some((seq, None)); }
        let reply = self.worker.as_ref()?.try_recv()?;
        Some((reply.seq, reply.result.ok()))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    /// Records what would be sent; replies are delivered by the test.
    #[derive(Clone, Default)]
    pub(crate) struct Fake(pub(crate) Rc<RefCell<(Vec<(u64, String)>, VecDeque<(u64, Option<Vec<String>>)>)>>);

    impl Fake {
        pub(crate) fn sent(&self) -> Vec<String> {
            self.0.borrow().0.iter().map(|(_, url)| url.clone()).collect()
        }

        /// Answer the latest request.
        pub(crate) fn reply(&self, suggestions: Option<&[&str]>) {
            let mut state = self.0.borrow_mut();
            let seq = state.0.last().expect("a request was sent").0;
            state.1.push_back((seq, suggestions.map(|list| list.iter().map(|s| s.to_string()).collect())));
        }
    }

    impl Transport for Fake {
        fn send(&mut self, seq: u64, url: &str) {
            self.0.borrow_mut().0.push((seq, url.into()));
        }

        fn recv(&mut self) -> Option<(u64, Option<Vec<String>>)> {
            self.0.borrow_mut().1.pop_front()
        }
    }

    pub(crate) fn fake() -> (Suggestions, Fake) {
        let fake = Fake::default();
        (Suggestions::new(Box::new(fake.clone())), fake)
    }

    fn request(profile: &str, text: &str) -> Request {
        Request { profile: profile.into(), url: format!("https://suggest.example.test/s?q={text}") }
    }

    /// One frame: replies, the bar's want, then anything due.
    fn frame(suggest: &mut Suggestions, now: f64, want: Option<Request>) -> Option<f64> {
        suggest.receive();
        suggest.want(now, want);
        suggest.send_due(now)
    }

    #[test]
    fn requests_wait_for_typing_to_pause_and_run_one_at_a_time() {
        let (mut suggest, fake) = fake();
        let wait = frame(&mut suggest, 0.0, Some(request("p", "r")));
        assert!(wait.is_some_and(|wait| (wait - DEBOUNCE).abs() < 1e-9), "{wait:?}");
        frame(&mut suggest, 0.1, Some(request("p", "ru")));
        assert_eq!(frame(&mut suggest, 0.2, Some(request("p", "ru"))).map(|w| (w * 100.0).round()), Some(5.0));
        assert!(fake.sent().is_empty(), "typing has not paused");
        assert_eq!(frame(&mut suggest, 0.26, Some(request("p", "ru"))), None);
        assert_eq!(fake.sent(), [request("p", "ru").url]);
        // More typing waits for the request in flight, however long the pause.
        frame(&mut suggest, 0.3, Some(request("p", "rust")));
        assert_eq!(frame(&mut suggest, 2.0, Some(request("p", "rust"))), None);
        assert_eq!(fake.sent().len(), 1);
        let before = suggest.revision();
        fake.reply(Some(&["ru stale"]));
        frame(&mut suggest, 2.1, Some(request("p", "rust")));
        assert!(suggest.shown(&request("p", "ru")).is_empty() && suggest.shown(&request("p", "rust")).is_empty());
        assert_eq!(suggest.revision(), before, "a stale reply changes nothing");
        assert_eq!(fake.sent(), [request("p", "ru").url, request("p", "rust").url]);
        fake.reply(Some(&["rust lang"]));
        frame(&mut suggest, 2.2, Some(request("p", "rust")));
        assert_eq!(suggest.shown(&request("p", "rust")), ["rust lang"]);
        assert_ne!(suggest.revision(), before);
        // An answered want is not asked again.
        frame(&mut suggest, 3.0, Some(request("p", "rust")));
        assert_eq!(fake.sent().len(), 2);
        // Typing back to the text in flight uses its reply and asks once.
        frame(&mut suggest, 4.0, Some(request("p", "serde")));
        frame(&mut suggest, 4.5, Some(request("p", "serde")));
        frame(&mut suggest, 4.6, Some(request("p", "serd")));
        frame(&mut suggest, 4.7, Some(request("p", "serde")));
        fake.reply(Some(&["serde json"]));
        frame(&mut suggest, 5.5, Some(request("p", "serde")));
        assert_eq!(suggest.shown(&request("p", "serde")), ["serde json"]);
        assert_eq!(fake.sent().len(), 3);
    }

    #[test]
    fn replies_are_dropped_after_more_typing_a_profile_switch_or_closing() {
        for change in [Some(request("p", "rust l")), Some(request("other", "rust")), None] {
            let (mut suggest, fake) = fake();
            frame(&mut suggest, 0.0, Some(request("p", "rust")));
            frame(&mut suggest, 1.0, Some(request("p", "rust")));
            assert_eq!(fake.sent().len(), 1);
            frame(&mut suggest, 1.1, change.clone());
            fake.reply(Some(&["rust lang"]));
            frame(&mut suggest, 1.12, change.clone());
            assert!(suggest.shown(&request("p", "rust")).is_empty(), "{change:?}");
            if let Some(change) = &change { assert!(suggest.shown(change).is_empty()); }
        }
        // Shown suggestions go away as soon as the want changes.
        let (mut suggest, fake) = fake();
        frame(&mut suggest, 0.0, Some(request("p", "rust")));
        frame(&mut suggest, 1.0, Some(request("p", "rust")));
        fake.reply(Some(&["rust lang"]));
        frame(&mut suggest, 1.1, Some(request("p", "rust")));
        let shown = suggest.revision();
        frame(&mut suggest, 1.2, None);
        assert!(suggest.shown(&request("p", "rust")).is_empty());
        assert_ne!(suggest.revision(), shown);
        // Reopening and typing the same text asks again.
        frame(&mut suggest, 5.0, Some(request("p", "rust")));
        frame(&mut suggest, 6.0, Some(request("p", "rust")));
        assert_eq!(fake.sent().len(), 2);
    }

    #[test]
    fn failures_show_nothing_are_not_retried_and_log_each_host_once() {
        let (mut suggest, fake) = fake();
        for (start, text) in [(0.0, "rust"), (2.0, "serde")] {
            frame(&mut suggest, start, Some(request("p", text)));
            frame(&mut suggest, start + 1.0, Some(request("p", text)));
            fake.reply(None);
            frame(&mut suggest, start + 1.1, Some(request("p", text)));
            frame(&mut suggest, start + 1.5, Some(request("p", text)));
            assert!(suggest.shown(&request("p", text)).is_empty());
        }
        assert_eq!(fake.sent().len(), 2, "one attempt per text");
        assert_eq!(suggest.logged.iter().collect::<Vec<_>>(), ["suggest.example.test"]);
    }
}
