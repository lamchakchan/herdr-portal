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
        fs::create_dir_all(dir.join("cfg")).unwrap();
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

    /// Saved machines as the fake `herdr machine list` prints them.
    fn machines(&self, rows: &str) {
        write_exec(
            &self.dir.join("bin/herdr"),
            &format!("#!/bin/sh\necho '{rows}'\n"),
        );
    }

    /// Like `assert_clean`, and the fake's own folder (state, config and log paths) never shows either.
    fn assert_clean(&self, what: &str, stdout: &str, stderr: &str) {
        assert_clean(what, stdout, stderr);
        let dir = self.dir.display().to_string();
        assert!(
            !stdout.contains(&dir),
            "{what}: stdout holds a path: {stdout}"
        );
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
        self.run_env(args, &[])
    }

    fn run_env(&self, args: &[&str], env: &[(&str, &str)]) -> (bool, String, String) {
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
            .env("HERDR_PLUGIN_CONFIG_DIR", self.dir.join("cfg"))
            .env("HERDR_PANE_ID", "w1:p1")
            .envs(env.iter().copied())
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

#[test]
fn watch_once_rows_hide_the_target_and_ssh_s_words() {
    for mode in ["down", "unreachable"] {
        let fake = Fake::new(&format!("watch-{mode}"), mode);
        let (out, err) = fake.run(&["watch", "--once", "--json"]);
        let doc: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        let devbox = doc["machines"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["machine"] == "devbox")
            .unwrap_or_else(|| panic!("no devbox row: {out}"))
            .clone();
        assert_eq!(devbox["ok"], false, "{out}");
        assert_eq!(devbox["error"]["code"], "ssh_failed", "{out}");
        fake.assert_clean(&format!("watch --once, {mode}"), &out, &err);
        // The person at the terminal still sees ssh's words.
        let (_, plain, _) = fake.run_status(&["watch", "--once"]);
        assert!(
            plain.contains("192.0.2.1"),
            "plain watch keeps the detail: {plain}"
        );
    }
}

#[test]
fn a_missing_herdr_binary_names_no_path() {
    let fake = Fake::new("noherdr", "unreachable");
    let missing = "/Users/someone/.local/bin/herdr";
    let (_, out, err) = fake.run_env(&["scan", "local", "--json"], &[("HERDR_BIN_PATH", missing)]);
    assert_eq!(error_code(&out), "herdr_failed");
    fake.assert_clean("scan, no herdr", &out, &err);
    let (_, out, err) = fake.run_env(&["requests", "--json"], &[("HERDR_BIN_PATH", missing)]);
    assert_eq!(error_code(&out), "herdr_failed");
    fake.assert_clean("requests, no herdr", &out, &err);
}

#[test]
fn an_auto_config_that_does_not_parse_names_no_path_or_contents() {
    let fake = Fake::new("badcfg", "unreachable");
    fs::write(
        fake.dir.join("cfg/config.toml"),
        "[auto]\nports = /Users/someone/secret\n",
    )
    .unwrap();
    for args in [
        &["watch", "--status", "--json"][..],
        &["watch", "--once", "--json"][..],
    ] {
        let (out, err) = fake.run(args);
        let doc: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(doc["ok"], true, "{out}");
        assert!(!out.contains("secret"), "{args:?}: {out}");
        fake.assert_clean(&format!("{args:?}"), &out, &err);
    }
    let (out, _) = fake.run(&["watch", "--status", "--json"]);
    let doc: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(doc["watch"]["auto"]["error"], "config.toml does not parse.");
    assert_eq!(doc["watch"]["auto"]["config"], "config.toml");
    assert_eq!(doc["watch"]["log"], "watch.log");
}

#[test]
fn restore_rejects_json() {
    let fake = Fake::new("restore", "down");
    fake.forward();
    let (ok, out, err) = fake.run_status(&["restore", "--json"]);
    assert!(!ok);
    assert_eq!(error_code(&out), "invalid_args");
    fake.assert_clean("restore --json", &out, &err);
}

#[test]
fn an_unknown_or_ambiguous_selector_is_not_echoed() {
    let fake = Fake::new("selector", "unreachable");
    let (out, err) = fake.run(&["scan", "someone@192.0.2.9", "--json"]);
    assert_eq!(error_code(&out), "machine_not_found");
    assert!(!out.contains("192.0.2.9"), "{out}");
    fake.assert_clean("unknown selector", &out, &err);
    fake.machines(&format!(
        r#"[{{"id":"a1","label":"twin","target":"{TARGET}","enabled":true}},{{"id":"b2","label":"twin","target":"{TARGET}","enabled":true}}]"#
    ));
    let (out, err) = fake.run(&["scan", "twin", "--json"]);
    assert_eq!(error_code(&out), "invalid_args");
    assert!(!out.contains("twin"), "{out}");
    fake.assert_clean("ambiguous selector", &out, &err);
}

#[test]
fn add_start_and_stop_failures_hide_the_target() {
    for mode in ["down", "unreachable"] {
        let fake = Fake::new(&format!("forward-{mode}"), mode);
        let (out, err) = fake.run(&["add", "devbox", "5173", "--local-port", "45173", "--json"]);
        assert_eq!(error_code(&out), "ssh_failed", "add, {mode}");
        fake.assert_clean(&format!("add, {mode}"), &out, &err);
        fake.forward();
        let (out, err) = fake.run(&["start", "5173", "--json"]);
        assert_eq!(error_code(&out), "ssh_failed", "start, {mode}: {out}");
        fake.assert_clean(&format!("start, {mode}"), &out, &err);
        let (out, err) = fake.run(&["stop", "5173", "--json"]);
        fake.assert_clean(&format!("stop, {mode}"), &out, &err);
    }
}

#[test]
fn request_commands_hide_herdr_s_words() {
    let fake = Fake::new("requests", "unreachable");
    write_exec(
        &fake.dir.join("bin/herdr"),
        &format!("#!/bin/sh\necho 'herdr: /Users/someone/.config {TARGET} refused' >&2\nexit 3\n"),
    );
    for args in [
        &["request", "5173", "--json"][..],
        &["release", "5173", "--json"][..],
        &["requests", "--json"][..],
    ] {
        let (out, err) = fake.run(args);
        assert_eq!(error_code(&out), "herdr_failed", "{args:?}: {out}");
        fake.assert_clean(&format!("{args:?}"), &out, &err);
    }
}
