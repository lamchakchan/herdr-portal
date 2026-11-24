//! `progress <machine> --json` end to end: the real script under bash against a fixture HOME of hostile files, run
//! for Local and through a fake `ssh` (which runs the script locally, as a remote shell would). Only the kept keys may
//! reach stdout; stderr stays empty; nothing names the SSH target; transcripts are stat'd and never read.

use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const TARGET: &str = "someone@192.0.2.1";
const LEAD: &str = "0f3c2a1e-5b6d-4e7f-8a9b-0c1d2e3f4a5b";
/// Text in the fixture that must never reach the output: a prompt, extra keys, a description, transcript content.
const NEVER: [&str; 5] = [
    "SENTINEL-PROMPT",
    "SENTINEL-EXTRA",
    "SENTINEL-DESC",
    "SENTINEL-TRANSCRIPT",
    "192.0.2.1",
];

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let dir = std::env::temp_dir().join(format!("hp-progress-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let home = dir.join("home");
        let progress = home.join(".herdr-projects/.progress");
        fs::create_dir_all(&progress).unwrap();
        fs::write(
            progress.join("w1_p2-1a2b3c4d.json"),
            r#"{"pane_id":"w1:p2","terminal_id":"t9","percent":40,"activity":"Fake remote activity SENTINEL-ACTIVITY","reported_at":1763999900,"session_started_at":1763990000,"prompt":"SENTINEL-PROMPT","extra":"SENTINEL-EXTRA"}"#,
        )
        .unwrap();
        fs::write(progress.join("w4_p1-9f8e7d6c.json"), "{not json").unwrap();
        fs::write(progress.join("w5_p1-00000000.json"), vec![b' '; 300 * 1024]).unwrap();
        let team = home.join(".claude/teams/squad");
        fs::create_dir_all(&team).unwrap();
        fs::write(
            team.join("config.json"),
            format!(r#"{{"leadSessionId":"{LEAD}","description":"SENTINEL-DESC","members":[{{"name":"impl-x","agentType":"general","joinedAt":1763990000000,"prompt":"SENTINEL-PROMPT"}}]}}"#),
        )
        .unwrap();
        let subagents = home.join(format!(".claude/projects/-Users-x-repo/{LEAD}/subagents"));
        fs::create_dir_all(&subagents).unwrap();
        fs::write(
            subagents.join("agent-aimpl-x-0123456789abcdef.jsonl"),
            "SENTINEL-TRANSCRIPT\n",
        )
        .unwrap();
        // A lead id that is not one: it must never become a path.
        fs::create_dir_all(home.join(".claude/teams/evil")).unwrap();
        fs::write(
            home.join(".claude/teams/evil/config.json"),
            r#"{"leadSessionId":"../../../../tmp","members":[]}"#,
        )
        .unwrap();

        fs::create_dir_all(dir.join("bin")).unwrap();
        // The fake ssh: `-O check` says the master runs; a command (`bash -s`) runs here, under the fixture HOME.
        write_exec(
            &dir.join("bin/ssh"),
            &format!(
                "#!/bin/sh\ncase \" $* \" in *\" -O check \"*) echo 'Master running (pid=1)' >&2; exit 0 ;; *\" -N \"*) exit 0 ;; esac\nHOME='{}' exec bash -s\n",
                home.display()
            ),
        );
        write_exec(
            &dir.join("bin/herdr"),
            &format!("#!/bin/sh\necho '[{{\"id\":\"5b4e\",\"label\":\"devbox\",\"target\":\"{TARGET}\",\"session\":\"\",\"enabled\":true}}]'\n"),
        );
        fs::create_dir_all(dir.join("state")).unwrap();
        Fixture { dir }
    }

    fn run(&self, args: &[&str]) -> (bool, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_herdr-portal"))
            .args(args)
            .env(
                "PATH",
                format!(
                    "{}:/usr/bin:/bin:/usr/sbin:/sbin",
                    self.dir.join("bin").display()
                ),
            )
            .env("HOME", self.dir.join("home"))
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

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn write_exec(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn check(machine: &str, machine_id: &str, label: &str) {
    let fixture = Fixture::new(machine);
    let (ok, stdout, stderr) = fixture.run(&["progress", machine, "--json"]);
    assert!(ok, "{stdout}{stderr}");
    assert_eq!(stderr, "", "nothing on stderr");
    for s in NEVER {
        assert!(!stdout.contains(s), "stdout holds {s}: {stdout}");
    }
    let doc: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(doc["ok"], true);
    assert_eq!(doc["schema"], 1);
    assert_eq!(doc["machine"], label);
    assert_eq!(doc["machine_id"], machine_id);
    assert!(doc["now"].as_i64().unwrap() > 1_700_000_000);
    assert_eq!(doc["truncated"], false);

    let progress = doc["progress"].as_array().unwrap();
    let by_file = |f: &str| {
        progress
            .iter()
            .find(|p| p["file"] == f)
            .unwrap_or_else(|| panic!("no {f}: {stdout}"))
            .clone()
    };
    let record = &by_file("w1_p2-1a2b3c4d.json")["record"];
    assert_eq!(record.as_object().unwrap().len(), 6);
    assert_eq!(record["activity"], "Fake remote activity SENTINEL-ACTIVITY");
    assert_eq!(by_file("w4_p1-9f8e7d6c.json")["error"], "bad_json");
    assert_eq!(by_file("w5_p1-00000000.json")["error"], "too_large");

    let teams = doc["teams"].as_array().unwrap();
    let squad = teams.iter().find(|t| t["team"] == "squad").unwrap();
    assert_eq!(
        squad["config"]["members"],
        serde_json::json!([{"name": "impl-x", "agentType": "general", "joinedAt": 1763990000000_i64}])
    );
    let transcripts = squad["transcripts"].as_array().unwrap();
    assert_eq!(transcripts.len(), 1);
    assert_eq!(
        transcripts[0]["name"],
        "agent-aimpl-x-0123456789abcdef.jsonl"
    );
    assert!(transcripts[0]["mtime_ms"].as_i64().unwrap() > 1_700_000_000_000);
    let evil = teams.iter().find(|t| t["team"] == "evil").unwrap();
    assert_eq!(evil["transcripts"], serde_json::json!([]));
}

#[test]
fn local_reads_this_host_and_keeps_only_the_agreed_keys() {
    check("local", "local", "Local");
}

#[test]
fn a_machine_is_read_over_its_ssh_master_and_named_by_label_only() {
    check("devbox", "5b4e", "devbox");
}

#[test]
fn a_home_without_the_folders_gives_empty_lists() {
    let fixture = Fixture::new("empty");
    fs::remove_dir_all(fixture.dir.join("home")).unwrap();
    fs::create_dir_all(fixture.dir.join("home")).unwrap();
    let (ok, stdout, stderr) = fixture.run(&["progress", "local", "--json"]);
    assert!(ok, "{stdout}{stderr}");
    let doc: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(doc["progress"], serde_json::json!([]));
    assert_eq!(doc["teams"], serde_json::json!([]));
}

#[test]
fn an_unknown_machine_names_no_selector_in_json() {
    let fixture = Fixture::new("unknown");
    let (ok, stdout, _) = fixture.run(&["progress", TARGET, "--json"]);
    // The target names the saved machine, so it resolves; an unknown one must not be echoed.
    assert!(ok, "{stdout}");
    let (ok, stdout, _) = fixture.run(&["progress", "nobody@198.51.100.7", "--json"]);
    assert!(!ok);
    assert!(
        stdout.contains("machine_not_found") && !stdout.contains("198.51.100.7"),
        "{stdout}"
    );
}
