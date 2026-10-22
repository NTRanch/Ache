//! Test-only loopback stub of the Lovense app: a throwaway python3 HTTP
//! server on an ephemeral 127.0.0.1 port that answers exactly one request
//! with a canned body, then exits. It reports what it received.

use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::process::{Child, ChildStdout, Command, Stdio};

const STUB_SCRIPT: &str = r#"
import sys, json, http.server
body = sys.argv[1].encode() * int(sys.argv[2])
status = int(sys.argv[3])
class Handler(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        received = self.rfile.read(length).decode("utf-8", "replace")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
        print(json.dumps({"path": self.path,
                          "platform": self.headers.get("X-platform"),
                          "body": received}), flush=True)
    def log_message(self, *args):
        pass
server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
server.timeout = 10
print(server.server_address[1], flush=True)
server.handle_request()
"#;

/// A running stub. Killed and reaped on drop.
pub struct AppStub {
    child: Child,
    stdout: BufReader<ChildStdout>,
    pub port: u16,
}

impl AppStub {
    /// Serve `body` repeated `repeat` times with HTTP `status`.
    pub fn start(body: &str, repeat: usize, status: u16) -> AppStub {
        let mut child = Command::new("python3")
            .arg("-I")
            .arg("-c")
            .arg(STUB_SCRIPT)
            .arg(body)
            .arg(repeat.to_string())
            .arg(status.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("python3 is required for the loopback stub");
        let mut stdout = BufReader::new(child.stdout.take().expect("stdout is piped"));
        let mut line = String::new();
        stdout.read_line(&mut line).expect("stub prints its port");
        let port = line.trim().parse().expect("stub port is a number");
        AppStub {
            child,
            stdout,
            port,
        }
    }

    /// The request the stub recorded, as JSON. Blocks until the stub answers.
    pub fn received(&mut self) -> serde_json::Value {
        let mut line = String::new();
        self.stdout
            .read_line(&mut line)
            .expect("stub reports the request");
        serde_json::from_str(&line).expect("stub report is JSON")
    }
}

impl Drop for AppStub {
    fn drop(&mut self) {
        // Best effort: the stub may already have exited after one request.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A loopback port with nothing listening on it (bound, then released).
pub fn closed_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("local addr").port()
}
