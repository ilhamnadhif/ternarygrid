//! A stand-in for an OpenAI-compatible service, for the tests that drive the AI assistant end
//! to end: each request gets the next reply, streamed as the real ones are.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

/// A reply that asks to call `tool` with `sql`.
pub fn tool_call(tool: &str, sql: &str) -> String {
    let arguments = serde_json::json!({ "sql": sql }).to_string();
    let delta = serde_json::json!({
        "choices": [{ "delta": { "tool_calls": [{ "index": 0, "id": "call_1", "type": "function", "function": { "name": tool, "arguments": arguments } }] }, "finish_reason": null }]
    });
    format!("data: {delta}\n\ndata: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"tool_calls\"}}]}}\n\ndata: [DONE]\n\n")
}

/// A reply that answers with `text`.
pub fn text(text: &str) -> String {
    let delta = serde_json::json!({ "choices": [{ "delta": { "content": text }, "finish_reason": null }] });
    format!("data: {delta}\n\ndata: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\ndata: [DONE]\n\n")
}

/// Serve `replies` one per request. Returns the base URL and the bodies of the requests.
pub fn serve(replies: Vec<String>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!("http://{}/v1", listener.local_addr().expect("addr"));
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let seen = bodies.clone();
    std::thread::spawn(move || {
        for reply in replies {
            let Ok((mut sock, _)) = listener.accept() else { return };
            seen.lock().unwrap().push(read_request(&mut sock));
            let head = format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", reply.len());
            let _ = sock.write_all(head.as_bytes());
            let _ = sock.write_all(reply.as_bytes());
        }
    });
    (base, bodies)
}

/// The body of the request on `sock`.
fn read_request(sock: &mut impl Read) -> String {
    let (mut buf, mut tmp) = (Vec::new(), [0u8; 8192]);
    let end = loop {
        let n = sock.read(&mut tmp).unwrap_or(0);
        if n == 0 {
            return String::new();
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
    let len: usize = head.lines().find_map(|l| l.strip_prefix("content-length:")).and_then(|v| v.trim().parse().ok()).unwrap_or(0);
    while buf.len() < end + len {
        let n = sock.read(&mut tmp).unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    String::from_utf8_lossy(&buf[end..]).to_string()
}
