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
        // A kept key whose value is an object: it could carry a dropped key, so it goes.
        fs::write(
            progress.join("w2_p1-2b3c4d5e.json"),
            r#"{"pane_id":"w2:p1","terminal_id":{"prompt":"SENTINEL-PROMPT"},"percent":5,"activity":"a","reported_at":1,"session_started_at":1}"#,
        )
        .unwrap();
        // A name that would break the framing and spoof the clock.
        fs::write(progress.join("x\nEND\nNOW\t1\nCAP\tx\nz.json"), "{}").unwrap();
        fs::write(progress.join("w5_p1-00000000.json"), vec![b' '; 300 * 1024]).unwrap();
        let team = home.join(".claude/teams/squad");
        fs::create_dir_all(&team).unwrap();
        fs::write(
            team.join("config.json"),
            format!(r#"{{"leadSessionId":"{LEAD}","description":"SENTINEL-DESC","members":[{{"name":"impl-x","agentType":"general","joinedAt":1763990000000,"prompt":"SENTINEL-PROMPT"}},{{"name":{{"prompt":"SENTINEL-PROMPT"}},"agentType":"g"}}]}}"#),
        )
        .unwrap();
        let subagents = home.join(format!(".claude/projects/-Users-x-repo/{LEAD}/subagents"));
        fs::create_dir_all(&subagents).unwrap();
        fs::write(
            subagents.join("agent-aimpl-x-0123456789abcdef.jsonl"),
            "SENTINEL-TRANSCRIPT\n",
        )
        .unwrap();
        fs::write(subagents.join("agent-ax\nNOW\t2\n.jsonl"), "").unwrap();
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
                "#!/bin/sh\ncase \" $* \" in *\" -O check \"*) echo 'Master running (pid=1)' >&2; exit 0 ;; *\" -N \"*) exit 0 ;; esac\nHOME='{}' exec bash -s 2>>'{}'\n",
                home.display(),
                dir.join("script-stderr").display()
            ),
        );
        write_exec(
            &dir.join("bin/herdr"),
            &format!("#!/bin/sh\necho '[{{\"id\":\"5b4e\",\"label\":\"devbox\",\"target\":\"{TARGET}\",\"session\":\"\",\"enabled\":true}}]'\n"),
        );
        fs::create_dir_all(dir.join("state")).unwrap();
        // Read only on the machine: a script that writes a temp file fails here, loudly, on stderr.
        fs::create_dir_all(dir.join("tmp")).unwrap();
        fs::set_permissions(dir.join("tmp"), fs::Permissions::from_mode(0o500)).unwrap();
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
            .env("TMPDIR", self.dir.join("tmp"))
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
        let _ = fs::set_permissions(self.dir.join("tmp"), fs::Permissions::from_mode(0o700));
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn write_exec(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Every file under `dir` with its size and modification time.
fn tree(dir: &Path) -> Vec<(PathBuf, u64, std::time::SystemTime)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap() {
            let e = e.unwrap();
            let meta = e.metadata().unwrap();
            if meta.is_dir() {
                stack.push(e.path());
            } else {
                out.push((e.path(), meta.len(), meta.modified().unwrap()));
            }
        }
    }
    out.sort();
    out
}

fn check(machine: &str, machine_id: &str, label: &str) {
    let fixture = Fixture::new(machine);
    let before = (
        tree(&fixture.dir.join("home")),
        tree(&fixture.dir.join("tmp")),
    );
    let (ok, stdout, stderr) = fixture.run(&["progress", machine, "--json"]);
    // Read only: nothing written in HOME or TMPDIR.
    assert_eq!(
        (
            tree(&fixture.dir.join("home")),
            tree(&fixture.dir.join("tmp"))
        ),
        before
    );
    assert!(ok, "{stdout}{stderr}");
    assert_eq!(stderr, "", "nothing on stderr");
    // The script's own stderr, which the binary drops on success; the fake ssh keeps it.
    let script_stderr = fs::read_to_string(fixture.dir.join("script-stderr")).unwrap_or_default();
    assert_eq!(script_stderr, "", "the script printed to stderr");
    for s in NEVER {
        assert!(!stdout.contains(s), "stdout holds {s}: {stdout}");
    }
    let doc: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(doc["ok"], true);
    assert_eq!(doc["schema"], 1);
    assert_eq!(doc["machine"], label);
    assert_eq!(doc["machine_id"], machine_id);
    assert!(doc["now"].as_i64().unwrap() > 1_700_000_000, "{stdout}");
    assert_eq!(doc["truncated"], false);

    let progress = doc["progress"].as_array().unwrap();
    let by_file = |f: &str| {
        progress
            .iter()
            .find(|p| p["file"] == f)
            .unwrap_or_else(|| panic!("no {f}: {stdout}"))
            .clone()
    };
    let mut files: Vec<&str> = progress
        .iter()
        .map(|p| p["file"].as_str().unwrap())
        .collect();
    files.sort();
    assert_eq!(
        files,
        [
            "w1_p2-1a2b3c4d.json",
            "w2_p1-2b3c4d5e.json",
            "w4_p1-9f8e7d6c.json",
            "w5_p1-00000000.json"
        ],
        "{stdout}"
    );
    assert!(by_file("w2_p1-2b3c4d5e.json")["record"]
        .get("terminal_id")
        .is_none());
    let record = &by_file("w1_p2-1a2b3c4d.json")["record"];
    assert_eq!(record.as_object().unwrap().len(), 6);
    assert_eq!(record["activity"], "Fake remote activity SENTINEL-ACTIVITY");
    assert_eq!(by_file("w4_p1-9f8e7d6c.json")["error"], "bad_json");
    assert_eq!(by_file("w5_p1-00000000.json")["error"], "too_large");

    let teams = doc["teams"].as_array().unwrap();
    let squad = teams.iter().find(|t| t["team"] == "squad").unwrap();
    assert_eq!(
        squad["config"]["members"],
        serde_json::json!([{"name": "impl-x", "agentType": "general", "joinedAt": 1763990000000_i64}, {"agentType": "g"}])
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

#[test]
fn a_full_records_folder_still_leaves_the_teams() {
    let fixture = Fixture::new("budget");
    let progress = fixture.dir.join("home/.herdr-projects/.progress");
    // Nine files past 256 KiB send no body; five of 250 KB pass the 1 MiB budget for records.
    for i in 0..9 {
        fs::write(
            progress.join(format!("big{i}.json")),
            vec![b' '; 300 * 1000],
        )
        .unwrap();
    }
    for i in 0..5 {
        fs::write(
            progress.join(format!("fill{i}.json")),
            vec![b' '; 250 * 1000],
        )
        .unwrap();
    }
    let (ok, stdout, stderr) = fixture.run(&["progress", "devbox", "--json"]);
    assert!(ok, "{stdout}{stderr}");
    let doc: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(doc["truncated"], true);
    let teams = doc["teams"].as_array().unwrap();
    assert!(teams.iter().any(|t| t["team"] == "squad"), "{stdout}");
    let large = doc["progress"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["error"] == "too_large")
        .count();
    assert!(large >= 9, "{stdout}");
    assert!(stdout.len() < 3 * 1024 * 1024);
}

#[test]
fn an_ambiguous_selector_names_no_target() {
    let fixture = Fixture::new("twin");
    write_exec(
        &fixture.dir.join("bin/herdr"),
        &format!("#!/bin/sh\necho '[{{\"id\":\"a1\",\"label\":\"x\",\"target\":\"{TARGET}\",\"enabled\":true}},{{\"id\":\"b2\",\"label\":\"y\",\"target\":\"{TARGET}\",\"enabled\":true}}]'\n"),
    );
    let (ok, stdout, _) = fixture.run(&["progress", TARGET, "--json"]);
    assert!(!ok);
    assert!(
        stdout.contains("invalid_args") && !stdout.contains("192.0.2.1"),
        "{stdout}"
    );
}

const LEAD2: &str = "2a2b2c2d-0000-4000-8000-000000000002";
const LEAD3: &str = "3a3b3c3d-0000-4000-8000-000000000003";
const NEST: &str = "4a4b4c4d-0000-4000-8000-000000000004";

fn subagents(fixture: &Fixture, lead: &str) -> PathBuf {
    let dir = fixture.dir.join(format!(
        "home/.claude/projects/-Users-x-repo/{lead}/subagents"
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn team_of(doc: &Value, name: &str) -> Value {
    doc["teams"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["team"] == name)
        .unwrap_or_else(|| panic!("no team {name}: {doc}"))
        .clone()
}

#[test]
fn a_lead_with_many_sub_agents_keeps_its_teammates_and_says_when_its_list_is_cut() {
    let fixture = Fixture::new("many");
    let teams = fixture.dir.join("home/.claude/teams");
    // "big": 600 other sub-agents and a teammate whose name sorts last.
    fs::create_dir_all(teams.join("big")).unwrap();
    fs::write(
        teams.join("big/config.json"),
        format!(r#"{{"leadSessionId":"{LEAD2}","members":[{{"name":"zz-last","agentType":"g","joinedAt":1}}]}}"#),
    )
    .unwrap();
    let dir = subagents(&fixture, LEAD2);
    for i in 0..600 {
        fs::write(
            dir.join(format!("agent-aother{i:04}-0123456789abcdef.jsonl")),
            "",
        )
        .unwrap();
    }
    fs::write(dir.join("agent-azz-last-0123456789abcdef.jsonl"), "").unwrap();
    // "huge": 1100 sub-agents, past the per-lead cap; its teammate wrote last, so it is among the newest kept.
    fs::create_dir_all(teams.join("huge")).unwrap();
    fs::write(
        teams.join("huge/config.json"),
        format!(r#"{{"leadSessionId":"{LEAD3}","members":[{{"name":"newest","agentType":"g","joinedAt":1}}]}}"#),
    )
    .unwrap();
    let dir = subagents(&fixture, LEAD3);
    for i in 0..1100 {
        fs::write(
            dir.join(format!("agent-aold{i:04}-0123456789abcdef.jsonl")),
            "",
        )
        .unwrap();
    }
    std::thread::sleep(std::time::Duration::from_millis(1100));
    fs::write(dir.join("agent-anewest-0123456789abcdef.jsonl"), "").unwrap();

    let (ok, stdout, stderr) = fixture.run(&["progress", "devbox", "--json"]);
    assert!(ok, "{stdout}{stderr}");
    let doc: Value = serde_json::from_str(&stdout).unwrap();
    let names = |t: &Value| -> Vec<String> {
        t["transcripts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["name"].as_str().unwrap().to_string())
            .collect()
    };
    let big = team_of(&doc, "big");
    assert!(names(&big).contains(&"agent-azz-last-0123456789abcdef.jsonl".to_string()));
    assert_eq!(big["transcripts_complete"], true);
    let huge = team_of(&doc, "huge");
    assert_eq!(huge["transcripts_complete"], false);
    assert_eq!(names(&huge)[0], "agent-anewest-0123456789abcdef.jsonl");
    // The other teams still get theirs.
    let squad = team_of(&doc, "squad");
    assert_eq!(squad["transcripts_complete"], true);
    assert_eq!(names(&squad), ["agent-aimpl-x-0123456789abcdef.jsonl"]);
}

#[test]
fn a_nested_lead_id_neither_hides_the_real_lead_nor_costs_another_team() {
    let fixture = Fixture::new("nested");
    let teams = fixture.dir.join("home/.claude/teams");
    fs::create_dir_all(teams.join("nested")).unwrap();
    fs::write(
        teams.join("nested/config.json"),
        format!(r#"{{"members":[{{"name":"n1","x":{{"leadSessionId":"{NEST}"}}}}],"leadSessionId":"{LEAD2}"}}"#),
    )
    .unwrap();
    fs::write(
        subagents(&fixture, LEAD2).join("agent-an1-0123456789abcdef.jsonl"),
        "",
    )
    .unwrap();
    let dir = subagents(&fixture, NEST);
    for i in 0..600 {
        fs::write(
            dir.join(format!("agent-ax{i:04}-0123456789abcdef.jsonl")),
            "",
        )
        .unwrap();
    }
    let (ok, stdout, stderr) = fixture.run(&["progress", "devbox", "--json"]);
    assert!(ok, "{stdout}{stderr}");
    let doc: Value = serde_json::from_str(&stdout).unwrap();
    let nested = team_of(&doc, "nested");
    assert_eq!(
        nested["transcripts"].as_array().unwrap().len(),
        1,
        "{nested}"
    );
    assert_eq!(nested["transcripts_complete"], true);
    assert_eq!(
        team_of(&doc, "squad")["transcripts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

const LEAD4: &str = "5a5b5c5d-0000-4000-8000-000000000005";

#[test]
fn a_stat_that_fails_leaves_the_list_incomplete() {
    // `stat` answers for "/" (so the script picks it) and fails for every transcript; then it fails for everything.
    for fake in [
        "#!/bin/sh\nfor a in \"$@\"; do last=\"$a\"; done\n[ \"$last\" = / ] && exec /usr/bin/stat \"$@\"\nexit 1\n",
        "#!/bin/sh\nexit 1\n",
    ] {
        let fixture = Fixture::new("badstat");
        write_exec(&fixture.dir.join("bin/stat"), fake);
        let (ok, stdout, stderr) = fixture.run(&["progress", "devbox", "--json"]);
        assert!(ok, "{stdout}{stderr}");
        let doc: Value = serde_json::from_str(&stdout).unwrap();
        let squad = team_of(&doc, "squad");
        assert_eq!(squad["transcripts_complete"], false, "{squad}");
    }
}

#[test]
fn only_the_top_level_lead_id_is_read() {
    let fixture = Fixture::new("toplevel");
    let teams = fixture.dir.join("home/.claude/teams");
    fs::create_dir_all(teams.join("deep")).unwrap();
    let nested: Vec<String> = (0..5)
        .map(|i| format!(r#"{{"name":"n{i}","x":{{"leadSessionId":"{NEST}"}}}}"#))
        .collect();
    fs::write(
        teams.join("deep/config.json"),
        format!(
            r#"{{"members":[{}],"leadSessionId":"{LEAD4}"}}"#,
            nested.join(",")
        ),
    )
    .unwrap();
    fs::write(
        subagents(&fixture, LEAD4).join("agent-an0-0123456789abcdef.jsonl"),
        "",
    )
    .unwrap();
    let (ok, stdout, stderr) = fixture.run(&["progress", "devbox", "--json"]);
    assert!(ok, "{stdout}{stderr}");
    let doc: Value = serde_json::from_str(&stdout).unwrap();
    let deep = team_of(&doc, "deep");
    assert_eq!(deep["transcripts_complete"], true, "{deep}");
    assert_eq!(deep["transcripts"].as_array().unwrap().len(), 1, "{deep}");
}

/// A fake ssh that runs `body` in place of the script (after the master checks pass).
fn with_ssh(fixture: &Fixture, body: &str) {
    write_exec(
        &fixture.dir.join("bin/ssh"),
        &format!("#!/bin/sh\ncase \" $* \" in *\" -O check \"*) echo 'Master running (pid=1)' >&2; exit 0 ;; *\" -N \"*) exit 0 ;; esac\n{body}\n"),
    );
}

#[test]
fn a_machine_that_floods_is_cut_off() {
    let fixture = Fixture::new("flood");
    with_ssh(&fixture, "cat >/dev/null; exec yes");
    let start = std::time::Instant::now();
    let (ok, stdout, _) = fixture.run(&["progress", "devbox", "--json"]);
    assert!(!ok);
    assert!(stdout.contains("printed too much"), "{stdout}");
    assert!(start.elapsed() < std::time::Duration::from_secs(10));
}

/// Runs `progress devbox --json` with the debug build's 1 s deadline.
#[cfg(debug_assertions)]
fn run_with_short_deadline(fixture: &Fixture) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_herdr-portal"))
        .args(["progress", "devbox", "--json"])
        .env(
            "PATH",
            format!(
                "{}:/usr/bin:/bin:/usr/sbin:/sbin",
                fixture.dir.join("bin").display()
            ),
        )
        .env("HOME", fixture.dir.join("home"))
        .env("HERDR_BIN_PATH", fixture.dir.join("bin/herdr"))
        .env("HERDR_PLUGIN_STATE_DIR", fixture.dir.join("state"))
        .env("TMPDIR", fixture.dir.join("tmp"))
        .env("HERDR_PORTAL_PROGRESS_DEADLINE_MS", "1000")
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[cfg(debug_assertions)]
fn gone(pidfile: &std::path::Path) -> bool {
    let pid: i32 = fs::read_to_string(pidfile).unwrap().trim().parse().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));
    !Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .unwrap()
        .success()
}

/// Debug builds only: the deadline knob is ignored in release, where the read has its full 20 s.
#[cfg(debug_assertions)]
#[test]
fn a_machine_that_hangs_or_leaves_its_output_open_is_cut_off_with_everything_it_started() {
    for (name, body) in [
        ("hang", "cat >/dev/null; sleep 60 & echo $! > '{pid}'; wait"),
        // ssh exits at once, but what it left behind still holds stdout.
        (
            "linger",
            "cat >/dev/null; sleep 60 & echo $! > '{pid}'; exit 0",
        ),
    ] {
        let fixture = Fixture::new(name);
        let pidfile = fixture.dir.join("sleep.pid");
        with_ssh(
            &fixture,
            &body.replace("{pid}", &pidfile.display().to_string()),
        );
        let start = std::time::Instant::now();
        let (ok, stdout) = run_with_short_deadline(&fixture);
        assert!(!ok, "{name}");
        assert!(stdout.contains("took too long"), "{name}: {stdout}");
        assert!(
            start.elapsed() < std::time::Duration::from_secs(10),
            "{name}"
        );
        assert!(gone(&pidfile), "{name}: the fake ssh's sleep still runs");
    }
}

#[test]
fn a_transcript_name_that_could_forge_a_time_is_skipped_and_leaves_the_list_incomplete() {
    let fixture = Fixture::new("forge");
    // Printed whole by stat, this name would give agent-avictim a write time of 9999999999.5.
    fs::write(
        subagents(&fixture, LEAD)
            .join("agent-ax\n9999999999.5\tagent-avictim-0123456789abcdef.jsonl"),
        "",
    )
    .unwrap();
    let (ok, stdout, stderr) = fixture.run(&["progress", "devbox", "--json"]);
    assert!(ok, "{stdout}{stderr}");
    assert!(!stdout.contains("victim"), "{stdout}");
    let doc: Value = serde_json::from_str(&stdout).unwrap();
    let squad = team_of(&doc, "squad");
    assert_eq!(squad["transcripts_complete"], false, "{squad}");
    assert_eq!(
        squad["transcripts"][0]["name"],
        "agent-aimpl-x-0123456789abcdef.jsonl"
    );
}
