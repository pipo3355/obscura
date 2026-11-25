// `fetch --wait` follows navigations the page starts after load. A challenge
// page posts a solution, sets a cookie, and reloads itself from a timer; the
// dump must be the document served for the reload, not the challenge.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const CHALLENGE: &str = r#"<!doctype html><title>check</title><p id="s">checking</p><script>
document.addEventListener('DOMContentLoaded', function () {
  if (document.cookie.indexOf('solved=1') >= 0) {
    setTimeout(function () { window.location = location.href; }, 3000);
  }
});
fetch('/verify', {method: 'POST', body: 'nonce=1'}).then(function (r) { return r.json(); }).then(function () {
  document.cookie = 'solved=1; Path=/; Max-Age=10';
  document.getElementById('s').textContent = 'passed, redirecting';
  setTimeout(function () { window.location = location.href; }, 300);
});
</script>"#;

const LISTING: &str =
    r#"<!doctype html><title>listing</title><div data-marker="item">phone</div>"#;

struct Server {
    address: String,
    requests: Arc<Mutex<Vec<String>>>,
}

impl Server {
    fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture");
        listener.set_nonblocking(true).expect("set fixture nonblocking");
        let address = listener.local_addr().expect("fixture address").to_string();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&requests);
        thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(60);
            while Instant::now() < deadline {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let seen = Arc::clone(&seen);
                        thread::spawn(move || serve(stream, &seen));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("accept fixture connection: {error}"),
                }
            }
        });
        Self { address, requests }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.address, path)
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().expect("fixture requests").clone()
    }
}

fn serve(mut stream: TcpStream, seen: &Arc<Mutex<Vec<String>>>) {
    stream.set_nonblocking(false).expect("set fixture connection blocking");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set fixture read timeout");
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];
    while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(count) => request.extend_from_slice(&buffer[..count]),
        }
    }
    let request = String::from_utf8_lossy(&request).to_string();
    let mut words = request.lines().next().unwrap_or_default().split_whitespace();
    let method = words.next().unwrap_or_default().to_string();
    let path = words.next().unwrap_or("/").to_string();
    let solved = request.lines().any(|line| {
        line.to_ascii_lowercase().starts_with("cookie:") && line.contains("solved=1")
    });
    seen.lock()
        .expect("fixture requests")
        .push(format!("{method} {path}{}", if solved { " solved" } else { "" }));
    let (content_type, body) = match (method.as_str(), path.as_str()) {
        ("POST", "/verify") => {
            // Resolve well after load, like a proof-of-work round trip.
            thread::sleep(Duration::from_millis(800));
            ("application/json", r#"{"ok":true}"#)
        }
        (_, "/list") if solved => ("text/html", LISTING),
        (_, "/list") => ("text/html", CHALLENGE),
        _ => ("text/plain", "not found"),
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

fn obscura(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_obscura"))
        .args(args)
        .output()
        .expect("run obscura CLI")
}

#[test]
fn fetch_wait_follows_a_timer_reload_after_a_solved_challenge() {
    let server = Server::spawn();
    let url = server.url("/list");
    let started = Instant::now();
    let output = obscura(&[
        "--allow-private-network",
        "fetch",
        "--wait",
        "5",
        "--dump",
        "html",
        &url,
    ]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(stdout.contains(r#"data-marker="item""#), "stdout: {stdout}\nstderr: {stderr}");
    assert!(stderr.contains("Page navigated:"), "stderr: {stderr}");
    let requests = server.requests();
    assert_eq!(
        requests,
        vec!["GET /list", "POST /verify", "GET /list solved"],
        "the reload carries the cookie set by the challenge"
    );
    // The landed listing keeps the rest of the fixed wait; nothing extends it.
    assert!(started.elapsed() < Duration::from_secs(15), "took {:?}", started.elapsed());
}
