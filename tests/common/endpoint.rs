//! A scriptable introspection endpoint: a `TcpListener` in this process that
//! answers each request with the next reply in its script (repeating the last
//! once it runs out), and counts what it received.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone)]
pub(crate) enum Reply {
    /// 200 with a one-field schema whose field is named this.
    Schema(&'static str),
    /// Any status and body, plus extra header lines (`Name: value`).
    Status(u16, &'static str, &'static [&'static str]),
    /// Hang up without answering.
    Close,
    /// Wait, then answer with the schema.
    Slow(Duration, &'static str),
}

pub(crate) struct Endpoint {
    pub(crate) url: String,
    script: Arc<Mutex<VecDeque<Reply>>>,
    hits: Arc<Mutex<usize>>,
}

impl Endpoint {
    pub(crate) fn start(script: &[Reply]) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
        let url = format!(
            "http://{}/graphql",
            listener.local_addr().expect("an address")
        );
        let ep = Endpoint {
            url,
            script: Arc::new(Mutex::new(script.iter().cloned().collect())),
            hits: Arc::default(),
        };
        let (script, hits) = (Arc::clone(&ep.script), Arc::clone(&ep.hits));
        // Daemon by convention: the harness ends the process, which ends this.
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let reply = {
                    let mut s = script.lock().expect("script");
                    let next = s.front().cloned().expect("a non-empty script");
                    if s.len() > 1 {
                        s.pop_front();
                    }
                    next
                };
                *hits.lock().expect("hits") += 1;
                std::thread::spawn(move || answer(stream, reply));
            }
        });
        ep
    }

    pub(crate) fn hits(&self) -> usize {
        *self.hits.lock().expect("hits")
    }
}

fn schema_body(field: &str) -> String {
    serde_json::json!({
        "data": { "__schema": {
            "queryType": { "name": "Query" },
            "types": [{
                "kind": "OBJECT",
                "name": "Query",
                "fields": [{
                    "name": field,
                    "args": [],
                    "type": { "kind": "SCALAR", "name": "String" },
                    "isDeprecated": false
                }]
            }],
            "directives": []
        }}
    })
    .to_string()
}

fn answer(mut stream: TcpStream, reply: Reply) {
    let mut rd = BufReader::new(stream.try_clone().expect("a second handle"));
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        if rd.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap_or(0);
            }
        }
    }
    let mut body = vec![0u8; length];
    let _ = rd.read_exact(&mut body);

    let (status, payload, extra): (u16, String, &[&str]) = match reply {
        Reply::Schema(f) => (200, schema_body(f), &[]),
        Reply::Status(code, body, extra) => (code, body.to_string(), extra),
        Reply::Close => return,
        Reply::Slow(wait, f) => {
            std::thread::sleep(wait);
            (200, schema_body(f), &[])
        }
    };
    let mut head = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        payload.len()
    );
    for h in extra {
        head.push_str(h);
        head.push_str("\r\n");
    }
    let _ = write!(stream, "{head}\r\n{payload}");
}
