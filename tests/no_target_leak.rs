//! With `--json`, the SSH target and ssh's stderr never reach the portal's output: a JSON caller (Headway, an agent)
//! may show an error's message, and ssh's stderr can name the user, the host or a home path. The plain CLI keeps that
//! detail for the person at the terminal. A fake `ssh` and a fake `herdr` on PATH stand in for the real ones: no
//! network, no real machine.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const TARGET: &str = "someone@192.0.2.1";
/// What a hostile or chatty ssh, or a failing remote command, might print.
const LEAKS: [&str; 4] = ["192.0.2.1", "someone", "/home/", "/Users/"];

struct Fake {
    dir: PathBuf,
}

impl Fake {
    /// `mode` steers the fake ssh: `down` (no master ever), `unreachable` (master up, the command exits 255),
    /// `remote-fails` (master up, the command exits 1), `garbled` (`-O check` passes with unreadable output).
    fn new(name: &str, mode: &str) -> Fake {
        let dir = std::env::temp_dir().join(format!("hp-leak-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("bin")).unwrap();
        fs::create_dir_all(dir.join("state")).unwrap();
        let ssh = format!(
            r#"#!/bin/sh
for a in "$@"; do last="$a"; done
case " $* " in
  *" -O check "*)
    case "{mode}" in
      down) echo "Control socket connect(/tmp/hpl-x): No such file or directory for {TARGET}" >&2; exit 255 ;;
      garbled) echo "mux_client_hello_exchange: {TARGET} /home/someone/.ssh" >&2; exit 0 ;;
      *) echo "Master running (pid=1)" >&2; exit 0 ;;
    esac ;;
  *" -N "*) exit 0 ;;
esac
cat >/dev/null
case "{mode}" in
  remote-fails) echo "bash: /home/someone/bin: {TARGET} permission denied" >&2; exit 1 ;;
  *) echo "ssh: connect to host {TARGET} port 22: Connection refused" >&2; exit 255 ;;
esac
"#
        );
        let herdr = format!(
            "#!/bin/sh\necho '[{{\"id\":\"5b4e\",\"label\":\"devbox\",\"target\":\"{TARGET}\",\"session\":\"\",\"enabled\":true}}]'\n"
        );
        write_exec(&dir.join("bin/ssh"), &ssh);
        write_exec(&dir.join("bin/herdr"), &herdr);
        Fake { dir }
    }

    fn forward(&self) {
        let rows = format!(
            r#"[{{"machine_id":"5b4e","label":"devbox","target":"{TARGET}","remote_port":5173,"local_port":5173}}]"#
        );
        fs::write(self.dir.join("state/forwards.json"), rows).unwrap();
    }

    fn run(&self, args: &[&str]) -> (String, String) {
        let (_, out, err) = self.run_status(args);
        (out, err)
    }

    fn run_status(&self, args: &[&str]) -> (bool, String, String) {
        let path = format!(
            "{}:/usr/bin:/bin:/usr/sbin:/sbin",
            self.dir.join("bin").display()
        );
        let out = Command::new(env!("CARGO_BIN_EXE_herdr-portal"))
            .args(args)
            .env("PATH", path)
            .env("HOME", &self.dir)
            .env("HERDR_BIN_PATH", self.dir.join("bin/herdr"))
            .env("HERDR_PLUGIN_STATE_DIR", self.dir.join("state"))
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn write_exec(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn error_code(stdout: &str) -> String {
    let doc: serde_json::Value =
        serde_json::from_str(stdout.trim()).unwrap_or_else(|_| panic!("not JSON: {stdout}"));
    assert_eq!(doc["ok"], false, "{stdout}");
    doc["error"]["code"].as_str().unwrap().to_string()
}

fn assert_clean(what: &str, stdout: &str, stderr: &str) {
    for leak in LEAKS {
        assert!(
            !stdout.contains(leak),
            "{what}: stdout holds {leak:?}: {stdout}"
        );
        assert!(
            !stderr.contains(leak),
            "{what}: stderr holds {leak:?}: {stderr}"
        );
    }
}

#[test]
fn scan_when_the_master_never_starts() {
    let fake = Fake::new("down", "down");
    let (out, err) = fake.run(&["scan", "devbox", "--json"]);
    assert_eq!(error_code(&out), "ssh_failed");
    assert_clean("scan, down", &out, &err);
}

#[test]
fn scan_when_ssh_cannot_reach_the_machine() {
    let fake = Fake::new("unreachable", "unreachable");
    let (out, err) = fake.run(&["scan", "devbox", "--json"]);
    assert_eq!(error_code(&out), "ssh_failed");
    assert_clean("scan, unreachable", &out, &err);
}

#[test]
fn scan_when_the_remote_command_fails() {
    let fake = Fake::new("remote", "remote-fails");
    let (out, err) = fake.run(&["scan", "devbox", "--json"]);
    assert_eq!(error_code(&out), "internal");
    assert_clean("scan, remote fails", &out, &err);
}

#[test]
fn list_when_the_master_check_is_unreadable() {
    let fake = Fake::new("garbled", "garbled");
    fake.forward();
    let (out, err) = fake.run(&["list", "--json"]);
    assert_eq!(error_code(&out), "internal");
    assert_clean("list, garbled", &out, &err);
}

#[test]
fn the_plain_cli_keeps_ssh_s_own_words() {
    let fake = Fake::new("plain", "unreachable");
    let (ok, _, err) = fake.run_status(&["scan", "devbox"]);
    assert!(!ok);
    assert!(
        err.contains("Connection refused"),
        "the person at the terminal sees why: {err}"
    );
}
