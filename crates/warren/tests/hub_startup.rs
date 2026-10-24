//! Exercise real CLI environment handling without changing the test process env.
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("warren-startup-{}", rand::random::<u64>()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Hub(Child);

impl Drop for Hub {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn startup(dir: &TestDir, token: Option<&str>, address: Option<&str>) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_warren"));
    cmd.args([
        "hub",
        "--listen",
        "127.0.0.1:0",
        "--proxy-listen",
        "127.0.0.1:0",
    ])
    .arg("--db")
    .arg(dir.0.join("warren.db"))
    .arg("--tls-cert-dir")
    .arg(&dir.0)
    .env_remove("WARREN_ENROLL_TOKEN")
    .env_remove("WARREN_PUBLIC_NODE_ADDR")
    .env_remove("WARREN_ADMIN_LISTEN")
    .env("WARREN_PROXY_USER", "test")
    .env("WARREN_PROXY_PASS", "test")
    .env("RUST_LOG", "info")
    .stdout(Stdio::piped())
    .stderr(Stdio::inherit());
    if let Some(token) = token {
        cmd.env("WARREN_ENROLL_TOKEN", token);
    }
    if let Some(address) = address {
        cmd.env("WARREN_PUBLIC_NODE_ADDR", address);
    }
    let mut hub = Hub(cmd.spawn().unwrap());
    let stdout = hub.0.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if tx.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    let mut output = String::new();
    loop {
        let line = rx
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .unwrap_or_else(|err| panic!("hub startup failed: {err}; output:\n{output}"));
        output.push_str(&line);
        output.push('\n');
        if line.contains("warren hub up") {
            break;
        }
    }
    drop(hub);
    reader.join().unwrap();
    output
}

fn printed<'a>(output: &'a str, prefix: &str) -> &'a str {
    output
        .lines()
        .find_map(|line| line.trim().strip_prefix(prefix))
        .unwrap()
        .trim()
}

#[test]
fn unset_and_empty_tokens_generate_and_reuse_a_secret() {
    for token in [None, Some("")] {
        let dir = TestDir::new();
        let output = startup(&dir, token, None);
        let secret = printed(&output, "enroll token:");
        assert!(!secret.is_empty());
        let restarted = startup(&dir, token, None);
        assert_eq!(printed(&restarted, "enroll token:"), secret);
        let store = warren::store::Store::open(dir.0.join("warren.db").to_str().unwrap()).unwrap();
        assert_eq!(store.list_tokens().unwrap().len(), 1);
        assert!(!store.token_valid("").unwrap());
    }
}

#[test]
fn join_command_contains_public_address_token_and_tls_pin() {
    for token in [Some("explicit-secret"), Some("")] {
        let dir = TestDir::new();
        let output = startup(&dir, token, Some("hub.example.com:7000"));
        let code = printed(&output, "join a device: warren node run --join ");
        let join = warren::joincode::decode(code).unwrap();
        assert_eq!(join.hub, "hub.example.com:7000");
        assert!(join.tls);
        assert_eq!(
            join.fingerprint.as_deref(),
            Some(printed(&output, "fingerprint:"))
        );
        let secret = printed(&output, "enroll token:");
        assert!(!secret.is_empty());
        assert_eq!(join.token.as_deref(), Some(secret));
        if token == Some("explicit-secret") {
            assert_eq!(secret, "explicit-secret");
        }
    }
}

#[test]
fn unset_and_empty_public_addresses_omit_join_command() {
    for address in [None, Some("")] {
        let output = startup(&TestDir::new(), None, address);
        assert!(!output.contains("join a device:"));
        assert!(output.contains("set --public-node-addr"));
    }
}

#[test]
fn existing_empty_token_is_not_migrated() {
    let dir = TestDir::new();
    let store = warren::store::Store::open(dir.0.join("warren.db").to_str().unwrap()).unwrap();
    store.add_token("", "cli").unwrap();
    startup(&dir, Some(""), None);
    assert_eq!(store.list_tokens().unwrap().len(), 1);
    assert!(store.token_valid("").unwrap());
}
