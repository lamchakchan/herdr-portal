//! macOS keeps `lsof` in /usr/sbin, and a caller such as Headway may start the portal with a PATH that has no /usr/sbin.
//! The listener checks must still find it there rather than fall back to `ss`, which macOS doesn't have. Each test runs
//! the real binary with PATH = a fake `ssh` and `herdr`, then /usr/bin:/bin. It is skipped where it can't show the bug:
//! no /usr/sbin/lsof, or an `lsof` that this PATH finds anyway.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const TARGET: &str = "someone@192.0.2.1";
const SYSTEM_PATH: &str = "/usr/bin:/bin";

struct Fake {
    dir: PathBuf,
}

impl Fake {
    fn new(name: &str) -> Fake {
        let dir = std::env::temp_dir().join(format!("hp-lsof-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for sub in ["bin", "state", "cfg"] {
            fs::create_dir_all(dir.join(sub)).unwrap();
        }
        // The master is up as pid 1, which owns no forward: the forward reads as stopped, not as an error.
        write_exec(
            &dir.join("bin/ssh"),
            "#!/bin/sh\ncase \" $* \" in *\" -O check \"*) echo 'Master running (pid=1)' >&2; exit 0 ;; esac\nexit 0\n",
        );
        // `machine list` answers with one saved machine; the workspace, agent and pane lists a scan of this Mac reads are empty.
        write_exec(
            &dir.join("bin/herdr"),
            &format!(
                "#!/bin/sh\ncase \" $* \" in\n  *\" machine list \"*) echo '[{{\"id\":\"5b4e\",\"label\":\"devbox\",\"target\":\"{TARGET}\",\"session\":\"\",\"enabled\":true}}]' ;;\n  *) echo '{{\"result\":{{}}}}' ;;\nesac\n"
            ),
        );
        Fake { dir }
    }

    fn forward(&self) {
        let rows = format!(
            r#"[{{"machine_id":"5b4e","label":"devbox","target":"{TARGET}","remote_port":5173,"local_port":5173}}]"#
        );
        fs::write(self.dir.join("state/forwards.json"), rows).unwrap();
    }

    fn run(&self, args: &[&str]) -> (bool, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_herdr-portal"))
            .args(args)
            .env(
                "PATH",
                format!("{}:{SYSTEM_PATH}", self.dir.join("bin").display()),
            )
            .env("HOME", &self.dir)
            .env("HERDR_BIN_PATH", self.dir.join("bin/herdr"))
            .env("HERDR_PLUGIN_STATE_DIR", self.dir.join("state"))
            .env("HERDR_PLUGIN_CONFIG_DIR", self.dir.join("cfg"))
            .env("HERDR_PANE_ID", "w1:p1")
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

/// Whether this machine can show the bug: lsof in /usr/sbin only, not on the test's PATH.
fn lsof_only_in_usr_sbin() -> bool {
    Path::new("/usr/sbin/lsof").exists()
        && !SYSTEM_PATH
            .split(':')
            .any(|dir| Path::new(dir).join("lsof").exists())
}

#[test]
fn list_checks_a_saved_forward_with_lsof_from_usr_sbin_when_path_lacks_it() {
    if !lsof_only_in_usr_sbin() {
        eprintln!("skipped: needs /usr/sbin/lsof and no lsof in {SYSTEM_PATH}");
        return;
    }
    let fake = Fake::new("list");
    fake.forward();
    let (ok, out, err) = fake.run(&["list", "--json"]);
    assert!(
        ok,
        "list --json failed without /usr/sbin on PATH: {out}{err}"
    );
    let doc: serde_json::Value =
        serde_json::from_str(out.trim()).unwrap_or_else(|_| panic!("not JSON: {out}"));
    assert_eq!(doc["ok"], true, "{out}");
}

#[test]
fn scan_of_this_mac_runs_lsof_from_usr_sbin_when_path_lacks_it() {
    if !lsof_only_in_usr_sbin() {
        eprintln!("skipped: needs /usr/sbin/lsof and no lsof in {SYSTEM_PATH}");
        return;
    }
    let fake = Fake::new("scan");
    let (ok, out, err) = fake.run(&["scan", "Local", "--json"]);
    assert!(
        ok,
        "scan Local --json failed without /usr/sbin on PATH: {out}{err}"
    );
    let doc: serde_json::Value =
        serde_json::from_str(out.trim()).unwrap_or_else(|_| panic!("not JSON: {out}"));
    assert_eq!(doc["ok"], true, "{out}");
}
