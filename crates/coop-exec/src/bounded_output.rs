use crate::{Sink, Stream, StreamTelemetry};
use coop_types::{MAX_OUTPUT_BYTES_PER_STREAM, MAX_OUTPUT_LINES, MAX_OUTPUT_RECORD_BYTES};
use sha2::{Digest, Sha256};

/// Fixed-memory output decoder shared by both execution backends.
///
/// Raw bytes are always hashed and counted, including bytes discarded after
/// the retention boundary. Only a small partial record is retained, so an
/// attacker can never make the server allocate an unterminated line.
pub(crate) struct BoundedOutput {
    stream: Stream,
    partial: Vec<u8>,
    raw_bytes: u64,
    emitted_bytes: u64,
    records: usize,
    truncated: bool,
    hasher: Sha256,
}

impl BoundedOutput {
    pub(crate) fn new(stream: Stream) -> Self {
        Self {
            stream,
            // UTF-8 replacement may expand each invalid byte to three bytes.
            // Keeping at most one third of the public record budget makes the
            // emitted String bounded without silently cutting a code point.
            partial: Vec::with_capacity(MAX_OUTPUT_RECORD_BYTES / 3),
            raw_bytes: 0,
            emitted_bytes: 0,
            records: 0,
            truncated: false,
            hasher: Sha256::new(),
        }
    }

    pub(crate) fn push(&mut self, bytes: &[u8], sink: &dyn Sink) {
        self.raw_bytes = self.raw_bytes.saturating_add(bytes.len() as u64);
        self.hasher.update(bytes);

        // Once retention is exhausted, continue hashing and draining without
        // retaining or decoding attacker-controlled bytes.
        if self.truncated {
            return;
        }

        let record_raw_cap = (MAX_OUTPUT_RECORD_BYTES / 3).max(1);
        for &byte in bytes {
            if byte == b'\n' {
                self.emit_partial(sink);
                if self.truncated {
                    break;
                }
                continue;
            }

            self.partial.push(byte);
            if self.partial.len() >= record_raw_cap {
                // Deterministically split overlong logical lines instead of
                // buffering until a newline that may never arrive.
                self.emit_partial(sink);
                if self.truncated {
                    break;
                }
            }
        }
    }

    pub(crate) fn finish(&mut self, sink: &dyn Sink) {
        if !self.partial.is_empty() && !self.truncated {
            self.emit_partial(sink);
        }
    }

    pub(crate) fn telemetry(&self) -> StreamTelemetry {
        StreamTelemetry {
            bytes_seen: self.raw_bytes,
            bytes_emitted: self.emitted_bytes,
            records_emitted: self.records as u64,
            sha256: hex(self.hasher.clone().finalize().as_slice()),
            truncated: self.truncated,
        }
    }

    fn emit_partial(&mut self, sink: &dyn Sink) {
        if self.records >= MAX_OUTPUT_LINES {
            self.mark_truncated(sink);
            self.partial.clear();
            return;
        }

        // Normalize platform CRLF to the line-oriented API's historical LF
        // semantics. The raw stream hash and byte count still cover `\r`.
        if self.partial.last() == Some(&b'\r') {
            self.partial.pop();
        }
        let text = String::from_utf8_lossy(&self.partial).into_owned();
        self.partial.clear();

        let remaining = MAX_OUTPUT_BYTES_PER_STREAM.saturating_sub(self.emitted_bytes as usize);
        if remaining == 0 {
            self.mark_truncated(sink);
            return;
        }

        if text.len() <= remaining {
            self.emitted_bytes += text.len() as u64;
            self.records += 1;
            sink.output(self.stream, text);
            return;
        }

        let mut end = remaining.min(text.len());
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        if end > 0 {
            let retained = text[..end].to_string();
            self.emitted_bytes += retained.len() as u64;
            self.records += 1;
            sink.output(self.stream, retained);
        }
        self.mark_truncated(sink);
    }

    fn mark_truncated(&mut self, sink: &dyn Sink) {
        if !self.truncated {
            self.truncated = true;
            sink.truncated(self.stream);
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(TABLE[(byte >> 4) as usize] as char);
        out.push(TABLE[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Collect {
        lines: Mutex<Vec<String>>,
        truncations: Mutex<usize>,
    }

    impl Sink for Collect {
        fn output(&self, _stream: Stream, line: String) {
            self.lines.lock().expect("lines").push(line);
        }

        fn violation(&self, _rule: &'static str, _detail: Value) {}

        fn truncated(&self, _stream: Stream) {
            *self.truncations.lock().expect("truncations") += 1;
        }
    }

    #[test]
    fn unterminated_output_is_record_and_byte_bounded() {
        let sink = Collect::default();
        let mut output = BoundedOutput::new(Stream::Stdout);
        let chunk = vec![b'a'; 8192];
        for _ in 0..1024 {
            output.push(&chunk, &sink);
        }
        output.finish(&sink);

        let telemetry = output.telemetry();
        assert_eq!(telemetry.bytes_seen, 8 * 1024 * 1024);
        assert!(telemetry.bytes_emitted <= MAX_OUTPUT_BYTES_PER_STREAM as u64);
        assert!(telemetry.truncated);
        assert_eq!(*sink.truncations.lock().unwrap(), 1);
        assert!(sink
            .lines
            .lock()
            .unwrap()
            .iter()
            .all(|line| line.len() <= MAX_OUTPUT_RECORD_BYTES));
    }

    #[test]
    fn invalid_utf8_cannot_expand_past_budget() {
        let sink = Collect::default();
        let mut output = BoundedOutput::new(Stream::Stderr);
        let chunk = vec![0xff; 8192];
        for _ in 0..256 {
            output.push(&chunk, &sink);
        }
        output.finish(&sink);
        let telemetry = output.telemetry();
        assert!(telemetry.bytes_emitted <= MAX_OUTPUT_BYTES_PER_STREAM as u64);
        assert_eq!(*sink.truncations.lock().unwrap(), 1);
    }
}
