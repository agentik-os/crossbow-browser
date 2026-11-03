//! Bounded text-only Chat Completions SSE decoding, not a general SSE client.
//! A valid answer needs index 0, `stop`, a dispatched `[DONE]`, and clean EOF.
//! Usage is allowed once, on the stop choice or a following empty-choice chunk.
//! Further choices, repeated DONE or data after DONE fail.
//! Comments/metadata never dispatch; there is no retry, reconnect or event-ID use.

use crate::chat::{valid_text, MAX_PROMPT_BYTES};
use crate::local::{LocalError, MAX_RESPONSE_BYTES};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
struct Chunk {
    choices: Vec<Choice>,
    #[serde(default)] usage: Value,
    #[serde(default)] error: Value,
}

#[derive(Deserialize)]
struct Choice {
    index: u64,
    delta: Delta,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Delta {
    #[serde(default, deserialize_with = "present_value")] role: Option<Value>,
    content: Option<String>,
    #[serde(default)] tool_calls: Value,
    #[serde(default)] function_call: Value,
    #[serde(default)] refusal: Value,
    #[serde(default)] reasoning_content: Value,
}

// Missing role is allowed, but an explicitly null/unsupported role is not.
// Typed envelopes also reject duplicate known fields instead of overwriting a
// tool payload, finish reason or role with a later benign-looking value.
fn present_value<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

// Allow an entire 16 KiB answer even when every character is JSON-escaped.
// Two fixed-capacity framing buffers avoid both unbounded growth and repeated
// prefix draining/rescanning. JSON parsing is capped per event and in aggregate.
const MAX_EVENT_BYTES: usize = 128 * 1024;
const MAX_EVENTS: usize = 8192;

pub(super) struct Decoder {
    line: Vec<u8>,
    data: Vec<u8>,
    has_data: bool,
    first_line: bool,
    after_cr: bool,
    event_bytes: usize,
    received: usize,
    events: usize,
    text: String,
    stopped: bool,
    usage_seen: bool,
    done: bool,
}

impl Decoder {
    pub(super) fn new() -> Self {
        Self {
            line: Vec::with_capacity(MAX_EVENT_BYTES), data: Vec::with_capacity(MAX_EVENT_BYTES),
            has_data: false, first_line: true, after_cr: false, event_bytes: 0, received: 0, events: 0,
            text: String::with_capacity(MAX_PROMPT_BYTES), stopped: false, usage_seen: false, done: false,
        }
    }

    /// `on_text` receives the full accumulated, still-uncommitted answer. It
    /// must return promptly; synchronous caller callbacks cannot be preempted.
    pub(super) fn push(&mut self, bytes: &[u8], on_text: &mut impl FnMut(&str)) -> Result<(), LocalError> {
        self.received = self.received.saturating_add(bytes.len());
        if self.received > MAX_RESPONSE_BYTES { return Err(LocalError::TooLarge); }
        for &byte in bytes {
            self.event_bytes += 1;
            if self.event_bytes > MAX_EVENT_BYTES { return Err(LocalError::TooLarge); }
            // CR terminates immediately; ignore just the LF of a CRLF pair,
            // even when that LF arrives in the next transport packet.
            if self.after_cr && byte == b'\n' { self.after_cr = false; continue; }
            self.after_cr = byte == b'\r';
            if matches!(byte, b'\r' | b'\n') {
                self.end_line(on_text)?;
                self.line.clear();
            } else {
                self.line.push(byte);
            }
        }
        Ok(())
    }

    fn end_line(&mut self, on_text: &mut impl FnMut(&str)) -> Result<(), LocalError> {
        // Complete lines preserve UTF-8 and an optional initial BOM across any
        // packet split. Strip only the stream's BOM, never answer text.
        let line = std::str::from_utf8(&self.line).map_err(|_| LocalError::InvalidResponse)?;
        let line = if self.first_line { line.strip_prefix('\u{feff}').unwrap_or(line) } else { line };
        self.first_line = false;
        if line.is_empty() {
            if self.has_data { self.dispatch(on_text)?; }
            self.data.clear();
            self.has_data = false;
            self.event_bytes = 0;
            return Ok(());
        }
        if line.chars().any(|c| c.is_control() && c != '\t') { return Err(LocalError::InvalidResponse); }
        if line.starts_with(':') { return Ok(()); }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        if field != "data" { return Ok(()); }
        let value = value.strip_prefix(' ').unwrap_or(value);
        let size = self.data.len().saturating_add(value.len()).saturating_add(usize::from(self.has_data));
        if size > MAX_EVENT_BYTES { return Err(LocalError::TooLarge); }
        if self.has_data { self.data.push(b'\n'); }
        self.data.extend_from_slice(value.as_bytes());
        self.has_data = true;
        Ok(())
    }

    fn dispatch(&mut self, on_text: &mut impl FnMut(&str)) -> Result<(), LocalError> {
        self.events += 1;
        if self.events > MAX_EVENTS { return Err(LocalError::TooLarge); }
        if self.done { return Err(LocalError::InvalidResponse); }
        if self.data.as_slice() == b"[DONE]" {
            if !self.stopped { return Err(LocalError::Incomplete); }
            if !valid_text(&self.text) { return Err(LocalError::InvalidResponse); }
            self.done = true;
            return Ok(());
        }
        let chunk: Chunk = serde_json::from_slice(&self.data).map_err(|_| LocalError::InvalidResponse)?;
        if !chunk.error.is_null() { return Err(LocalError::InvalidResponse); }
        if chunk.choices.is_empty() {
            if !self.stopped || self.usage_seen || !chunk.usage.is_object() { return Err(LocalError::InvalidResponse); }
            self.usage_seen = true;
            return Ok(());
        }
        if self.stopped || chunk.choices.len() != 1 { return Err(LocalError::InvalidResponse); }
        let choice = &chunk.choices[0];
        if !chunk.usage.is_null() {
            if self.usage_seen || !chunk.usage.is_object() || choice.finish_reason.as_deref() != Some("stop") { return Err(LocalError::InvalidResponse); }
            self.usage_seen = true;
        }
        if choice.index != 0 { return Err(LocalError::InvalidResponse); }
        let delta = &choice.delta;
        if delta.role.as_ref().is_some_and(|role| role.as_str() != Some("assistant"))
            || (!delta.tool_calls.is_null() && delta.tool_calls.as_array().is_none_or(|calls| !calls.is_empty()))
            || !delta.function_call.is_null() || !delta.refusal.is_null()
            || (!delta.reasoning_content.is_null() && delta.reasoning_content.as_str() != Some("")) {
            return Err(LocalError::InvalidResponse);
        }
        let content = delta.content.as_deref().unwrap_or("");
        if content.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')) { return Err(LocalError::InvalidResponse); }
        if self.text.len().saturating_add(content.len()) > MAX_PROMPT_BYTES { return Err(LocalError::TooLarge); }
        self.stopped = match choice.finish_reason.as_deref() {
            None => false,
            Some("stop") => true,
            Some(_) => return Err(LocalError::Incomplete),
        };
        if !content.is_empty() {
            self.text.push_str(content);
            on_text(&self.text);
        }
        Ok(())
    }

    pub(super) fn finish(self) -> Result<String, LocalError> {
        // SSE does not dispatch an unterminated event at EOF. Even a valid
        // prefix/DONE cannot hide a truncated tail or a failed HTTP transfer.
        if !self.done || !self.line.is_empty() || self.has_data { return Err(LocalError::Incomplete); }
        Ok(self.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(chunk: Value) -> String { format!("data: {chunk}\n\n") }

    fn content(text: &str, finish: Value) -> String {
        event(serde_json::json!({"choices":[{"index":0,"delta":{"role":"assistant","content":text},"finish_reason":finish}]}))
    }

    fn decode(bytes: &[u8]) -> Result<String, LocalError> {
        let mut decoder = Decoder::new();
        decoder.push(bytes, &mut |_| {})?;
        decoder.finish()
    }

    #[test]
    fn every_packet_split_preserves_utf8_crlf_metadata_and_multiline_data() {
        let body = concat!(
            ": keepalive\r\nevent: message\r\nid: ignored\r\nretry: 1\r\n\r\n",
            "data: {\r\ndata: \"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"é🦀\"},\"finish_reason\":null}]}\r\n\r\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\" done\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\r\n\r\n: ignored after done\n\n",
        ).as_bytes();
        for split in 0..=body.len() {
            let mut decoder = Decoder::new();
            let mut partials = Vec::new();
            let mut receive = |text: &str| partials.push(text.to_string());
            decoder.push(&body[..split], &mut receive).unwrap();
            decoder.push(&body[split..], &mut receive).unwrap();
            assert_eq!(decoder.finish(), Ok("é🦀 done".into()), "split {split}");
            assert_eq!(partials, ["é🦀", "é🦀 done"]);
        }
        let mut decoder = Decoder::new();
        for byte in body { decoder.push(std::slice::from_ref(byte), &mut |_| {}).unwrap(); }
        assert_eq!(decoder.finish(), Ok("é🦀 done".into()));
    }

    #[test]
    fn review_stream_initial_bom_preserves_first_delta_at_every_packet_split() {
        let body = format!("\u{feff}{}{}data: [DONE]\n\n", content("not ", Value::Null), content("safe", Value::String("stop".into())));
        for split in 0..=body.len() {
            let mut decoder = Decoder::new();
            decoder.push(&body.as_bytes()[..split], &mut |_| {}).unwrap();
            decoder.push(&body.as_bytes()[split..], &mut |_| {}).unwrap();
            assert_eq!(decoder.finish(), Ok("not safe".into()), "split {split}");
        }
        let mut decoder = Decoder::new();
        for byte in body.as_bytes() { decoder.push(std::slice::from_ref(byte), &mut |_| {}).unwrap(); }
        assert_eq!(decoder.finish(), Ok("not safe".into()));
        let text = "\u{feff}answer";
        assert_eq!(decode(format!("{}data: [DONE]\n\n", content(text, Value::String("stop".into()))).as_bytes()), Ok(text.into()), "only the framing BOM may be stripped");
    }

    #[test]
    fn review_stream_cr_lf_and_mixed_framing_survive_every_packet_split() {
        let event = content("é🦀", Value::String("stop".into()));
        for body in [
            format!("\u{feff}: metadata\r\r{}data: [DONE]\r\r", event.replace('\n', "\r")),
            format!(": metadata\r\n\r{}data: [DONE]\r\n\r", event.replace("\n\n", "\r\n\n")),
        ] {
            for split in 0..=body.len() {
                let mut decoder = Decoder::new();
                decoder.push(&body.as_bytes()[..split], &mut |_| {}).unwrap();
                decoder.push(&body.as_bytes()[split..], &mut |_| {}).unwrap();
                assert_eq!(decoder.finish(), Ok("é🦀".into()), "split {split}");
            }
            let mut decoder = Decoder::new();
            for byte in body.as_bytes() { decoder.push(std::slice::from_ref(byte), &mut |_| {}).unwrap(); }
            assert_eq!(decoder.finish(), Ok("é🦀".into()));
        }
    }

    #[test]
    fn review_stream_known_inert_reasoning_metadata_is_not_answer_text() {
        for value in [Value::Null, Value::String(String::new())] {
            let chunk = event(serde_json::json!({"choices":[{"index":0,"delta":{"content":"answer","reasoning_content":value},"finish_reason":"stop"}]}));
            assert_eq!(decode(format!("{chunk}data: [DONE]\n\n").as_bytes()), Ok("answer".into()));
        }
        for value in [serde_json::json!("nonempty"), serde_json::json!({}), serde_json::json!([]), serde_json::json!(3)] {
            let chunk = event(serde_json::json!({"choices":[{"index":0,"delta":{"content":"answer","reasoning_content":value},"finish_reason":"stop"}]}));
            let mut decoder = Decoder::new();
            assert_eq!(decoder.push(chunk.as_bytes(), &mut |_| panic!("unsupported payload exposed")), Err(LocalError::InvalidResponse));
        }
    }

    #[test]
    fn review_stream_terminal_usage_object_is_supported_exactly_once() {
        let usage = serde_json::json!({"prompt_tokens":1,"completion_tokens":1,"total_tokens":2});
        let stop = event(serde_json::json!({"choices":[{"index":0,"delta":{"content":"answer"},"finish_reason":"stop"}],"usage":usage}));
        assert_eq!(decode(format!("{stop}data: [DONE]\n\n").as_bytes()), Ok("answer".into()));
        let extra = event(serde_json::json!({"choices":[],"usage":usage}));
        assert_eq!(decode(format!("{stop}{extra}data: [DONE]\n\n").as_bytes()), Err(LocalError::InvalidResponse));
        for (finish, value) in [(Value::Null, usage), (serde_json::json!("stop"), serde_json::json!([])), (serde_json::json!("stop"), serde_json::json!(1))] {
            let chunk = event(serde_json::json!({"choices":[{"index":0,"delta":{"content":"answer"},"finish_reason":finish}],"usage":value}));
            let mut decoder = Decoder::new();
            assert_eq!(decoder.push(chunk.as_bytes(), &mut |_| panic!("invalid usage exposed text")), Err(LocalError::InvalidResponse));
        }
    }

    #[test]
    fn absent_null_and_empty_optional_text_metadata_are_supported() {
        for delta in [
            serde_json::json!({}),
            serde_json::json!({"role":"assistant","content":null}),
            serde_json::json!({"content":"","tool_calls":null}),
            serde_json::json!({"content":null,"tool_calls":[],"function_call":null,"refusal":null}),
        ] {
            let prefix = event(serde_json::json!({"choices":[{"index":0,"delta":delta,"finish_reason":null}],"usage":null}));
            let body = format!("{prefix}{}data: [DONE]\n\n", content(" plain <b>text</b> `code`\n\t ", Value::String("stop".into())));
            assert_eq!(decode(body.as_bytes()), Ok(" plain <b>text</b> `code`\n\t ".into()));
        }
    }

    #[test]
    fn only_one_usage_chunk_may_follow_stop_and_no_data_may_follow_done() {
        let stop = content("answer", Value::String("stop".into()));
        let usage = event(serde_json::json!({"choices":[],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}));
        assert_eq!(decode(format!("{stop}{usage}data: [DONE]\n\n").as_bytes()), Ok("answer".into()));
        for body in [
            format!("{usage}{stop}data: [DONE]\n\n"),
            format!("{stop}{usage}{usage}data: [DONE]\n\n"),
            format!("{stop}{}data: [DONE]\n\n", content("", Value::Null)),
            format!("{stop}data: {{\"choices\":[],\"usage\":null}}\n\ndata: [DONE]\n\n"),
            format!("{stop}data: [DONE]\n\n{usage}"),
            format!("{stop}data: [DONE]\n\ndata: [DONE]\n\n"),
        ] { assert_eq!(decode(body.as_bytes()), Err(LocalError::InvalidResponse), "{body}"); }
    }

    #[test]
    fn malformed_json_choices_roles_tools_and_controls_fail_without_dispatch() {
        for delta in [
            serde_json::json!({"role":"user","content":"bad"}),
            serde_json::json!({"role":null,"content":"bad"}),
            serde_json::json!({"content":[]}),
            serde_json::json!({"content":7}),
            serde_json::json!({"content":"bad\u{0000}"}),
            serde_json::json!({"content":"bad\u{0085}"}),
            serde_json::json!({"content":"bad","tool_calls":[{}]}),
            serde_json::json!({"content":"bad","tool_calls":{}}),
            serde_json::json!({"content":"bad","tool_calls":""}),
            serde_json::json!({"content":"bad","function_call":{}}),
            serde_json::json!({"content":"bad","function_call":[]}),
            serde_json::json!({"content":"bad","function_call":{"name":"run","arguments":"{}"}}),
            serde_json::json!({"content":"bad","refusal":"filtered"}),
            serde_json::json!({"content":"bad","audio":{}}),
        ] {
            let mut decoder = Decoder::new();
            let body = event(serde_json::json!({"choices":[{"index":0,"delta":delta,"finish_reason":null}]}));
            assert_eq!(decoder.push(body.as_bytes(), &mut |_| panic!("invalid delta was exposed")), Err(LocalError::InvalidResponse));
        }
        for body in [
            "data: not json\n\n", "data:\n\n", "data: []\n\n",
            "data: {\"error\":\"private server diagnostic\"}\n\n",
            "data: {\"choices\":[]}\n\n",
            "data: {\"choices\":[{\"index\":1,\"delta\":{}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{}}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{}},{\"index\":1,\"delta\":{}}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":true}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"length\",\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"user\",\"role\":\"assistant\"}}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{}],\"tool_calls\":null}}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"function_call\":{},\"function_call\":null}}]}\n\n",
            ": forbidden\u{0000}control\n\n",
        ] { assert_eq!(decode(body.as_bytes()), Err(LocalError::InvalidResponse), "{body}"); }
        assert_eq!(decode(b": bad \xff\n\n"), Err(LocalError::InvalidResponse));
        assert_eq!(decode(b"data: {\"choices\":\"\xf0\x9f\n\n"), Err(LocalError::InvalidResponse));
    }

    #[test]
    fn eof_and_filtered_or_missing_termination_never_commit_partial_text() {
        let partial = content("partial", Value::Null);
        let stopped = content("partial", Value::String("stop".into()));
        for body in [
            String::new(), partial.clone(), stopped.clone(), format!("{partial}data: [DONE]\n\n"),
            format!("{stopped}data: [DONE]\n"), format!("{stopped}data: [DONE]\n\ndata: "),
        ] { assert_eq!(decode(body.as_bytes()), Err(LocalError::Incomplete), "{body}"); }
        for reason in ["length", "content_filter", "tool_calls", "function_call", "unknown"] {
            assert_eq!(decode(content("partial", Value::String(reason.into())).as_bytes()), Err(LocalError::Incomplete));
        }
        for text in ["", " \r\n\t", "\u{2003}"] {
            assert_eq!(decode(format!("{}data: [DONE]\n\n", content(text, Value::String("stop".into()))).as_bytes()), Err(LocalError::InvalidResponse));
        }
    }

    #[test]
    fn decoded_output_event_buffers_event_work_and_total_wire_bytes_are_bounded() {
        let text = "é".repeat(MAX_PROMPT_BYTES / 2);
        assert_eq!(decode(format!("{}data: [DONE]\n\n", content(&text, Value::String("stop".into()))).as_bytes()), Ok(text.clone()));
        assert_eq!(decode(content(&format!("{text}x"), Value::Null).as_bytes()), Err(LocalError::TooLarge));
        assert_eq!(decode(format!("{}{}", content(&text, Value::Null), content("x", Value::Null)).as_bytes()), Err(LocalError::TooLarge));
        let mut decoder = Decoder::new();
        assert_eq!(decoder.push(&vec![b'x'; MAX_EVENT_BYTES], &mut |_| {}), Ok(()));
        assert_eq!(decoder.line.len(), MAX_EVENT_BYTES);
        assert_eq!(decoder.line.capacity(), MAX_EVENT_BYTES);
        assert_eq!(decoder.data.capacity(), MAX_EVENT_BYTES);
        assert_eq!(decoder.push(b"x", &mut |_| {}), Err(LocalError::TooLarge));
        let mut decoder = Decoder::new();
        let data_line = format!("data: {}\n", " ".repeat(MAX_EVENT_BYTES - 7));
        decoder.push(data_line.as_bytes(), &mut |_| {}).unwrap();
        assert_eq!(decoder.data.len(), MAX_EVENT_BYTES - 7);
        assert_eq!(decoder.data.capacity(), MAX_EVENT_BYTES);
        assert_eq!(decoder.push(b"\n", &mut |_| {}), Err(LocalError::TooLarge));
        let mut decoder = Decoder::new();
        let noop = event(serde_json::json!({"choices":[{"index":0,"delta":{}}]}));
        for _ in 0..MAX_EVENTS { decoder.push(noop.as_bytes(), &mut |_| {}).unwrap(); }
        assert_eq!(decoder.push(noop.as_bytes(), &mut |_| {}), Err(LocalError::TooLarge));
        let mut decoder = Decoder::new();
        let comments = b":\n\n".repeat(MAX_RESPONSE_BYTES / 3);
        decoder.push(&comments, &mut |_| panic!("comment dispatched")).unwrap();
        assert_eq!(decoder.push(b":\n\n", &mut |_| {}), Err(LocalError::TooLarge));
    }
}
