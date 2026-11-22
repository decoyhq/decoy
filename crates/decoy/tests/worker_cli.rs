//! `decoy serve` running Transformer workers: the handshake, forwarded stderr, a worker exiting
//! (which stops Decoy), and shutdown. The fake workers are shell scripts that print a `ready`
//! message prepared here.
#![cfg(unix)]

use std::fs;
use std::io::Read as _;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

const ROUTE: &str = r#"
id: hello
transport: http
match: { method: GET, path: /hello }
cases:
  any:
    behaviors:
      ok: { body: hi }
"#;

/// A project whose `decoy.yaml` runs `command` as the `env` worker.
fn project(command: &str) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("routes")).unwrap();
    fs::write(dir.path().join("routes/hello.yaml"), ROUTE).unwrap();
    fs::write(
        dir.path().join("collections.yaml"),
        "- id: main\n  routes: [hello:any:ok]\n",
    )
    .unwrap();
    fs::write(dir.path().join("ready.bin"), ready()).unwrap();
    fs::write(
        dir.path().join("decoy.yaml"),
        format!(
            "routes: routes\ncollections: collections.yaml\ntransformers:\n  env:\n    worker: [sh, -c, {command:?}]\n"
        ),
    )
    .unwrap();
    dir
}

fn serve(dir: &TempDir) -> Child {
    Command::new(env!("CARGO_BIN_EXE_decoy"))
        .args(["serve", "--config", "decoy.yaml", "--port", "0"])
        .current_dir(dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

/// Reads `child`'s stderr up to `listening on`, so a signal sent next finds Decoy serving.
fn wait_listening(child: &mut Child) {
    let stderr = child.stderr.as_mut().unwrap();
    let mut seen = Vec::new();
    let mut byte = [0u8];
    while !String::from_utf8_lossy(&seen).contains("listening on") {
        let read = stderr.read(&mut byte).unwrap();
        assert_eq!(
            read,
            1,
            "decoy exited early: {}",
            String::from_utf8_lossy(&seen)
        );
        seen.push(byte[0]);
    }
}

/// Waits for `child` to exit, up to `limit`.
fn exit_within(child: &mut Child, limit: Duration) -> (ExitStatus, String) {
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(
            started.elapsed() < limit,
            "decoy still running after {limit:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    (status, stderr)
}

#[test]
fn a_worker_that_exits_before_ready_fails_startup_with_its_stderr() {
    let dir = project("echo 'typedEnvelope: unknown option tags' >&2; exit 2");
    let (status, stderr) = exit_within(&mut serve(&dir), Duration::from_secs(10));
    assert!(!status.success());
    assert!(
        stderr.contains("Transformer worker `env` didn't start: "),
        "{stderr}"
    );
    assert!(
        stderr.contains("stderr:\n    typedEnvelope: unknown option tags"),
        "{stderr}"
    );
}

#[test]
fn a_worker_exiting_after_ready_stops_decoy_and_its_stderr_is_forwarded() {
    let dir = project("cat ready.bin; sleep 1; echo 'bye now' >&2; exit 3");
    let (status, stderr) = exit_within(&mut serve(&dir), Duration::from_secs(10));
    assert!(!status.success());
    assert!(stderr.contains("[env: fake@1.0.0] bye now"), "{stderr}");
    assert!(
        stderr.contains(
            "Transformer worker `env: fake@1.0.0` exited (exit status: 3); Decoy stops without it\n  stderr:\n    bye now"
        ),
        "{stderr}"
    );
}

#[test]
fn sigterm_stops_decoy_and_a_worker_that_ignores_shutdown() {
    let dir = project("cat ready.bin; exec sleep 60");
    let mut child = serve(&dir);
    wait_listening(&mut child);
    let killed = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let (status, stderr) = exit_within(&mut child, Duration::from_secs(10));
    assert!(status.success(), "{status}: {stderr}");
}

#[test]
fn worker_keys_take_no_separators() {
    let dir = project("true");
    let config = fs::read_to_string(dir.path().join("decoy.yaml")).unwrap();
    fs::write(
        dir.path().join("decoy.yaml"),
        config.replace("  env:", "  my/env:"),
    )
    .unwrap();
    let (status, stderr) = exit_within(&mut serve(&dir), Duration::from_secs(10));
    assert!(!status.success());
    assert!(
        stderr.contains("Transformer `my/env`: a key takes only letters, digits, `-` and `_`"),
        "{stderr}"
    );
}

/// `ready { protocol: 1, name: fake, version: 1.0.0, directions: [input] }`, framed.
fn ready() -> Vec<u8> {
    fn text(out: &mut Vec<u8>, text: &str) {
        out.push(0xa0 | u8::try_from(text.len()).unwrap());
        out.extend_from_slice(text.as_bytes());
    }
    let mut body = vec![0x85];
    text(&mut body, "kind");
    text(&mut body, "ready");
    text(&mut body, "protocol");
    body.push(1);
    text(&mut body, "name");
    text(&mut body, "fake");
    text(&mut body, "version");
    text(&mut body, "1.0.0");
    text(&mut body, "directions");
    body.push(0x91);
    text(&mut body, "input");
    let mut framed = u32::try_from(body.len()).unwrap().to_be_bytes().to_vec();
    framed.extend(body);
    framed
}
