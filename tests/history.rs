//! `event` and `history` end to end (rulings 1171, 1173, 1174): the real binary on a temp HOME and state dir, and a
//! fake `ssh` that runs the remote script under bash with another HOME. A good event appends one whole line at the
//! portal's own time; a bad one writes nothing and never echoes its value; files are 0600 in a 0700 directory; 20
//! writers at once leave only whole lines. The reader re-validates every line, keeps the newest within its caps, and
//! says `complete: false` whenever it dropped or cut anything.

use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const BIN: &str = env!("CARGO_BIN_EXE_herdr-portal");

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let dir = std::env::temp_dir().join(format!("hp-history-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("home")).unwrap();
        fs::create_dir_all(dir.join("state")).unwrap();
        // The remote machine: its own HOME, read by a fake ssh that runs the script locally as a remote shell would.
        fs::create_dir_all(dir.join("remote-home")).unwrap();
        fs::create_dir_all(dir.join("bin")).unwrap();
        write_exec(
            &dir.join("bin/ssh"),
            &format!(
                "#!/bin/sh\ncase \" $* \" in *\" -O check \"*) echo 'Master running (pid=1)' >&2; exit 0 ;; *\" -N \"*) exit 0 ;; esac\nfor a in \"$@\"; do last=$a; done\nHOME='{}' exec env -u XDG_STATE_HOME bash -s -- \"$last\" 2>>'{}'\n",
                dir.join("remote-home").display(),
                dir.join("script-stderr").display()
            ),
        );
        write_exec(
            &dir.join("bin/herdr"),
            &format!("#!/bin/sh\necho '[{{\"id\":\"5b4e\",\"label\":\"devbox\",\"target\":\"{TARGET}\",\"session\":\"\",\"enabled\":true}}]'\n"),
        );
        // Read only on the machine: a script that writes a temp file fails, loudly, on stderr.
        fs::create_dir_all(dir.join("tmp")).unwrap();
        fs::set_permissions(dir.join("tmp"), fs::Permissions::from_mode(0o500)).unwrap();
        Fixture { dir }
    }

    /// The remote machine's history directory, at the default state path.
    fn remote_history(&self) -> PathBuf {
        self.dir
            .join("remote-home/.local/state/herdr/plugins/lamchakchan.herdr-portal/history")
    }

    /// `history …` with the fake ssh and herdr on PATH; the parsed JSON (or the error JSON) and whether it succeeded.
    fn read(&self, args: &[&str], extra: &[(&str, &str)]) -> (bool, Value) {
        let mut all = vec!["history"];
        all.extend_from_slice(args);
        all.push("--json");
        let mut c = Command::new(BIN);
        c.args(&all)
            .env_clear()
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
            .env("TMPDIR", self.dir.join("tmp"));
        for (k, v) in extra {
            c.env(k, v);
        }
        let out = c.output().unwrap();
        let doc = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)));
        (out.status.success(), doc)
    }

    fn history(&self) -> PathBuf {
        self.dir.join("state/history")
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOME", self.dir.join("home"))
            .env("HERDR_PLUGIN_STATE_DIR", self.dir.join("state"));
        c
    }

    /// `script` under sh, with the binary as `$0` and the same environment.
    fn spawn_sh(&self, script: &str) -> std::process::Child {
        Command::new("/bin/sh")
            .args(["-c", script, BIN])
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOME", self.dir.join("home"))
            .env("HERDR_PLUGIN_STATE_DIR", self.dir.join("state"))
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    /// Every line of every month file, in file then line order.
    fn lines(&self) -> Vec<String> {
        let mut files: Vec<PathBuf> = match fs::read_dir(self.history()) {
            Ok(dir) => dir.map(|e| e.unwrap().path()).collect(),
            Err(_) => return Vec::new(),
        };
        files.sort();
        files
            .iter()
            .flat_map(|f| {
                fs::read_to_string(f)
                    .unwrap()
                    .lines()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::set_permissions(self.dir.join("tmp"), fs::Permissions::from_mode(0o700));
        let _ = fs::remove_dir_all(&self.dir);
    }
}

const TARGET: &str = "someone@192.0.2.1";

fn write_exec(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// The UTC month file for `at`, by the system's `date` (BSD `-r`, else GNU `-d @`).
fn month_of(at: u64) -> String {
    let bsd = Command::new("date")
        .args(["-u", "-r", &at.to_string(), "+%Y-%m"])
        .output()
        .unwrap();
    let out = if bsd.status.success() {
        bsd
    } else {
        Command::new("date")
            .args(["-u", "-d", &format!("@{at}"), "+%Y-%m"])
            .output()
            .unwrap()
    };
    format!("{}.jsonl", String::from_utf8(out.stdout).unwrap().trim())
}

/// A valid `fix` line for `work` at `at`, newline included.
fn fix_line(work: &str, at: u64, round: u64) -> String {
    format!(
        "{}\n",
        serde_json::json!({"schema":1,"at":at,"kind":"fix","work":work,"round":round})
    )
}

/// Appends `text` to the month file for `at` under `dir`.
fn put(dir: &Path, at: u64, text: &str) {
    use std::io::Write;
    fs::create_dir_all(dir).unwrap();
    fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(dir.join(month_of(at)))
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
}

/// The `(work, round)` of each event in an answer, in order.
fn rounds(doc: &Value) -> Vec<(String, u64)> {
    doc["events"]
        .as_array()
        .unwrap_or_else(|| panic!("{doc}"))
        .iter()
        .map(|e| {
            (
                e["work"].as_str().unwrap().to_string(),
                e["round"].as_u64().unwrap(),
            )
        })
        .collect()
}

const DAY: u64 = 86_400;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

const START: [&str; 12] = [
    "event",
    "start",
    "--work",
    "AT-3400",
    "--name",
    "impl-filter",
    "--role",
    "builder",
    "--model",
    "claude-opus-5-5",
    "--effort",
    "high",
];

#[test]
fn event_appends_one_line_at_the_portals_time_to_the_utc_month_file() {
    let fixture = Fixture::new("append");
    let before = now();
    let out = fixture.run(&START);
    let after = now();
    assert!(out.status.success(), "{out:?}");
    assert!(out.stdout.is_empty() && out.stderr.is_empty(), "{out:?}");
    let month = Command::new("date")
        .args(["-u", "+%Y-%m"])
        .output()
        .unwrap();
    let month = String::from_utf8(month.stdout).unwrap();
    let names: Vec<String> = fs::read_dir(fixture.history())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, [format!("{}.jsonl", month.trim())]);
    let lines = fixture.lines();
    assert_eq!(lines.len(), 1);
    let line: Value = serde_json::from_str(&lines[0]).unwrap();
    let at = line["at"].as_u64().unwrap();
    assert!((before..=after).contains(&at), "{at}");
    assert_eq!(
        line,
        serde_json::json!({"schema":1,"at":at,"kind":"start","work":"AT-3400","name":"impl-filter","role":"builder","model":"claude-opus-5-5","effort":"high"})
    );
    // A second event appends; it never rewrites.
    let out = fixture.run(&[
        "event",
        "pr",
        "--work",
        "AT-3400",
        "--repo",
        "glydways/headway",
        "--number",
        "298",
        "--action",
        "merged",
    ]);
    assert!(out.status.success(), "{out:?}");
    let lines = fixture.lines();
    assert_eq!(lines.len(), 2);
    assert!(
        lines[1].contains(r#""kind":"pr""#) && lines[1].contains(r#""number":298"#),
        "{}",
        lines[1]
    );
}

#[test]
fn json_output_echoes_the_event() {
    let fixture = Fixture::new("json");
    let mut args = START.to_vec();
    args.push("--json");
    let out = fixture.run(&args);
    assert!(out.status.success(), "{out:?}");
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["ok"], true);
    assert_eq!(doc["event"]["work"], "AT-3400");
    assert_eq!(
        serde_json::from_str::<Value>(&fixture.lines()[0]).unwrap(),
        doc["event"]
    );
}

#[test]
fn a_bad_event_writes_nothing_and_never_echoes_its_value() {
    let fixture = Fixture::new("bad");
    const SECRET: &str = "ZZ-SECRET";
    let cases: Vec<Vec<String>> = [
        vec!["event"],
        vec!["event", "merge", "--work", "AT-1"],
        vec!["event", "start", "end", "--work", "AT-1"],
        vec!["event", "end", "--outcome", "done"],
        vec![
            "event",
            "end",
            "--work",
            "AT-1",
            "--outcome",
            "done",
            "--at",
            "1800000000",
        ],
        vec!["event", "end", "--work", "AT-1", "--outcome", "ZZ-SECRET"],
        vec![
            "event",
            "end",
            "--work",
            "ZZ-SECRET\n{\"kind\":\"end\"}",
            "--outcome",
            "done",
        ],
        vec![
            "event",
            "end",
            "--work",
            "../../ZZ-SECRET",
            "--outcome",
            "done",
        ],
        vec![
            "event",
            "end",
            "--work",
            "AT-1",
            "--outcome",
            "done",
            "--name",
            "x",
        ],
        vec!["event", "fix", "--work", "AT-1", "--round", "+5"],
        vec!["event", "fix", "--work", "AT-1", "--round", "1x"],
        vec![
            "event",
            "fix",
            "--work",
            "AT-1",
            "--round",
            "99999999999999999999999",
        ],
        vec!["event", "fix", "--work", "AT-1", "--round", "0"],
        vec![
            "event", "pr", "--work", "AT-1", "--repo", "a/b", "--number", "-1", "--action",
            "opened",
        ],
        vec![
            "event",
            "ci",
            "--work",
            "AT-1",
            "--result",
            "pass",
            "--head",
            "ZZ-SECRET",
        ],
        vec![
            "event", "start", "--work", "AT-1", "--name", "x\ty", "--role", "builder", "--model",
            "opus",
        ],
        vec![
            "event",
            "start",
            "--work",
            "AT-1",
            "--name",
            "x",
            "--role",
            "builder",
            "--model",
            "opus",
            "--pane",
            "ZZ-SECRET",
        ],
        vec![
            "event",
            "start",
            "--work",
            "AT-1",
            "--name",
            "x",
            "--role",
            "builder",
            "--model",
            "opus",
            "--prompt",
            "ZZ-SECRET",
        ],
    ]
    .iter()
    .map(|c| c.iter().map(|s| s.to_string()).collect())
    .collect();
    for case in cases {
        for json in [false, true] {
            let mut args: Vec<&str> = case.iter().map(String::as_str).collect();
            if json {
                args.push("--json");
            }
            let out = fixture.run(&args);
            assert!(!out.status.success(), "{case:?} passed");
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(!text.contains(SECRET), "{case:?} echoed its value: {text}");
            if json {
                let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
                assert_eq!(doc["error"]["code"], "invalid_args", "{case:?}: {doc}");
            }
        }
    }
    assert_eq!(fixture.lines(), Vec::<String>::new());
}

#[test]
fn history_files_are_0600_in_a_0700_directory_even_one_made_looser_before() {
    let fixture = Fixture::new("modes");
    fs::create_dir_all(fixture.history()).unwrap();
    fs::set_permissions(fixture.history(), fs::Permissions::from_mode(0o755)).unwrap();
    let out = fixture.run(&START);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(mode(&fixture.history()), 0o700);
    for f in fs::read_dir(fixture.history()).unwrap() {
        assert_eq!(mode(&f.unwrap().path()), 0o600);
    }
}

#[test]
fn twenty_writers_at_once_leave_only_whole_lines() {
    let fixture = Fixture::new("concurrent");
    let children: Vec<_> = (0..20)
        .map(|w| {
            let script = format!(
                "i=1; while [ $i -le 50 ]; do \"$0\" event fix --work q-writer-{w} --round $i || exit 1; i=$((i + 1)); done"
            );
            fixture.spawn_sh(&script)
        })
        .collect();
    for child in children {
        let status = child.wait_with_output().unwrap();
        assert!(status.status.success(), "{status:?}");
    }
    let lines = fixture.lines();
    assert_eq!(lines.len(), 20 * 50);
    let mut seen = std::collections::HashSet::new();
    for line in &lines {
        let doc: Value = serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}"));
        assert_eq!(doc["kind"], "fix");
        assert!(
            seen.insert((doc["work"].to_string(), doc["round"].as_u64().unwrap())),
            "{line}"
        );
    }
}

#[test]
fn history_reads_across_a_month_boundary_and_filters_by_since() {
    for (name, remote) in [("months-local", false), ("months-remote", true)] {
        let fixture = Fixture::new(name);
        let machine = if remote { "devbox" } else { "Local" };
        let t = now();
        let dir = if remote {
            fixture.remote_history()
        } else {
            fixture.history()
        };
        // A month before the window: never read, so its garbage can't clear `complete`.
        put(&dir, t - 90 * DAY, "garbage from an old month\n");
        put(&dir, t - 60 * DAY, &fix_line("q-old", t - 60 * DAY, 1));
        put(&dir, t - 35 * DAY, &fix_line("q-a", t - 35 * DAY, 1));
        put(&dir, t - 3600, &fix_line("q-b", t - 3600, 2));
        assert_ne!(month_of(t - 35 * DAY), month_of(t - 3600));
        assert_ne!(month_of(t - 90 * DAY), month_of(t - 37 * DAY));
        let since = (t - 36 * DAY).to_string();
        let (ok, doc) = fixture.read(&[machine, "--since", &since], &[]);
        assert!(ok, "{doc}");
        assert_eq!(
            rounds(&doc),
            [("q-a".to_string(), 1), ("q-b".to_string(), 2)],
            "{name}"
        );
        assert_eq!(doc["complete"], true, "{name}: {doc}");
        assert_eq!(doc["machine"], machine);
        assert!((t..=now()).contains(&doc["now"].as_u64().unwrap()));
        let since = (t - 7200).to_string();
        let (_, doc) = fixture.read(&[machine, "--since", &since], &[]);
        assert_eq!(rounds(&doc), [("q-b".to_string(), 2)], "{name}");
        // The default window is the last 7 days.
        let (_, doc) = fixture.read(&[machine], &[]);
        assert_eq!(rounds(&doc), [("q-b".to_string(), 2)], "{name}");
    }
}

#[test]
fn a_bad_or_newer_line_is_dropped_and_clears_complete_while_good_lines_are_read() {
    let fixture = Fixture::new("badline");
    let t = now() - 3600;
    let good = |w: &str| fix_line(w, t, 1);
    for bad in [
        "not json\n".to_string(),
        format!(
            "{}\n",
            serde_json::json!({"schema":2,"at":t,"kind":"fix","work":"q-x","round":1})
        ),
        format!(
            "{}\n",
            serde_json::json!({"schema":1,"at":t,"kind":"merge","work":"q-x"})
        ),
        format!(
            "{}\n",
            serde_json::json!({"schema":1,"at":t,"kind":"fix","work":"q-x","round":1,"prompt":"ZZ-SECRET"})
        ),
        format!(
            "{}\n",
            serde_json::json!({"schema":1,"at":t,"kind":"fix","work":"ZZ SECRET","round":1})
        ),
    ] {
        let _ = fs::remove_dir_all(fixture.history());
        put(
            &fixture.history(),
            t,
            &format!("{}{bad}{}", good("q-a"), good("q-b")),
        );
        let (ok, doc) = fixture.read(&["Local"], &[]);
        assert!(ok, "{doc}");
        assert_eq!(
            rounds(&doc),
            [("q-a".to_string(), 1), ("q-b".to_string(), 1)],
            "{bad}"
        );
        assert_eq!(doc["complete"], false, "{bad}");
        assert!(!doc.to_string().contains("SECRET"), "{doc}");
    }
}

#[test]
fn a_partial_line_merged_with_the_next_is_dropped_and_later_lines_are_read() {
    // 1174: a write cut short leaves a partial line, and the next writer's line lands right after it.
    let fixture = Fixture::new("partial");
    let t = now() - 3600;
    let partial = r#"{"schema":1,"at":"#;
    put(
        &fixture.history(),
        t,
        &format!(
            "{}{partial}{}{}",
            fix_line("q-a", t, 1),
            fix_line("q-merged", t, 1),
            fix_line("q-c", t, 3)
        ),
    );
    let (ok, doc) = fixture.read(&["Local"], &[]);
    assert!(ok, "{doc}");
    assert_eq!(
        rounds(&doc),
        [("q-a".to_string(), 1), ("q-c".to_string(), 3)]
    );
    assert_eq!(doc["complete"], false);
    // A file that ends in a partial line loses only that line.
    let _ = fs::remove_dir_all(fixture.history());
    put(
        &fixture.history(),
        t,
        &format!("{}{partial}", fix_line("q-a", t, 1)),
    );
    let (_, doc) = fixture.read(&["Local"], &[]);
    assert_eq!(rounds(&doc), [("q-a".to_string(), 1)]);
    assert_eq!(doc["complete"], false);
}

#[test]
fn the_event_cap_keeps_the_newest_and_clears_complete() {
    let fixture = Fixture::new("eventcap");
    let t = now() - 3 * DAY;
    let text: String = (0..10_050u64)
        .map(|i| fix_line("q-a", t + i, 1 + i % 1000))
        .collect();
    put(&fixture.history(), t, &text);
    let (ok, doc) = fixture.read(&["Local"], &[]);
    assert!(ok);
    let events = doc["events"].as_array().unwrap();
    assert_eq!(events.len(), 10_000);
    assert_eq!(events[0]["at"].as_u64().unwrap(), t + 50);
    assert_eq!(events[9_999]["at"].as_u64().unwrap(), t + 10_049);
    assert_eq!(doc["complete"], false);
}

/// A valid `start` line of about 500 bytes.
fn big_line(at: u64, i: u64) -> String {
    format!(
        "{}\n",
        serde_json::json!({"schema":1,"at":at,"kind":"start","work":format!("herdr-portal:{}{i:08}", "b".repeat(100)),
            "name":"n".repeat(64),"role":"builder","model":"m".repeat(64),"pane_id":format!("{}:{}", "p".repeat(32), "q".repeat(32)),"terminal_id":"t".repeat(64)})
    )
}

#[test]
fn the_byte_cap_keeps_the_newest_months_and_clears_complete() {
    for (name, remote) in [("bytecap-local", false), ("bytecap-remote", true)] {
        let fixture = Fixture::new(name);
        let dir = if remote {
            fixture.remote_history()
        } else {
            fixture.history()
        };
        let t = now();
        put(
            &dir,
            t - 40 * DAY,
            &fix_line("q-older-month", t - 40 * DAY, 1),
        );
        let line = big_line(t - 3600, 0);
        let count = (3 * 1024 * 1024) / line.len() as u64;
        let text: String = (0..count).map(|i| big_line(t - 3600, i)).collect();
        // Older months must be outside this month for the test to mean anything.
        assert_ne!(
            month_of(t - 40 * DAY),
            month_of(t - 3600),
            "40 days back is always an earlier month"
        );
        put(&dir, t - 3600, &text);
        let since = (t - 41 * DAY).to_string();
        let (ok, doc) = fixture.read(
            &[if remote { "devbox" } else { "Local" }, "--since", &since],
            &[],
        );
        assert!(ok, "{doc}");
        let events = doc["events"].as_array().unwrap();
        assert_eq!(doc["complete"], false, "{name}");
        assert!(
            events.iter().all(|e| e["kind"] == "start"),
            "{name}: the older month should be cut"
        );
        // About 2 MiB of the newest lines; the cut first line is dropped, not mis-read.
        let kept = events.len() as u64;
        let per_mib = 1024 * 1024 / line.len() as u64;
        assert!(
            kept >= 2 * per_mib - 2 && kept <= 2 * per_mib,
            "{name}: kept {kept}"
        );
        let last = events.last().unwrap()["work"].as_str().unwrap().to_string();
        assert!(
            last.ends_with(&format!("{:08}", count - 1)),
            "{name}: {last}"
        );
    }
}

#[test]
fn a_missing_history_is_empty_and_complete() {
    let fixture = Fixture::new("missing");
    for machine in ["Local", "devbox"] {
        let (ok, doc) = fixture.read(&[machine], &[]);
        assert!(ok, "{doc}");
        assert_eq!(doc["events"], serde_json::json!([]));
        assert_eq!(doc["complete"], true);
    }
    assert!(
        !fixture.history().exists(),
        "the reader made the history directory"
    );
}

#[test]
fn a_machine_is_read_over_its_master_named_by_label_only_and_nothing_is_written_there() {
    let fixture = Fixture::new("remote");
    let t = now() - 3600;
    put(&fixture.remote_history(), t, &fix_line("q-remote", t, 4));
    put(&fixture.history(), t, &fix_line("q-local", t, 1));
    let before = tree(&fixture.dir.join("remote-home"));
    let (ok, doc) = fixture.read(&["devbox"], &[]);
    assert!(ok, "{doc}");
    assert_eq!(rounds(&doc), [("q-remote".to_string(), 4)]);
    assert_eq!(doc["machine"], "devbox");
    assert_eq!(doc["complete"], true);
    assert!(!doc.to_string().contains("192.0.2.1"), "{doc}");
    assert_eq!(tree(&fixture.dir.join("remote-home")), before);
    let stderr = fs::read_to_string(fixture.dir.join("script-stderr")).unwrap_or_default();
    assert_eq!(stderr, "", "the script wrote to stderr");
}

/// Every file under `dir` with its size and modification time.
fn tree(dir: &Path) -> Vec<(PathBuf, u64, SystemTime)> {
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

#[test]
fn a_bad_since_or_usage_is_invalid_args() {
    let fixture = Fixture::new("usage");
    let old = (now() - 401 * DAY).to_string();
    for args in [
        vec!["Local", "--since", "soon"],
        vec!["Local", "--since", "+5"],
        vec!["Local", "--since", &old],
        vec![],
        vec!["Local", "devbox"],
    ] {
        let (ok, doc) = fixture.read(&args, &[]);
        assert!(!ok, "{args:?}");
        assert_eq!(doc["error"]["code"], "invalid_args", "{args:?}: {doc}");
    }
}

#[test]
fn ssh_failure_is_ssh_failed_with_fixed_text() {
    let fixture = Fixture::new("sshfail");
    write_exec(
        &fixture.dir.join("bin/ssh"),
        "#!/bin/sh\ncase \" $* \" in *\" -O check \"*) echo 'Master running (pid=1)' >&2; exit 0 ;; *\" -N \"*) exit 0 ;; esac\necho 'someone@192.0.2.1: Connection refused' >&2\nexit 255\n",
    );
    let (ok, doc) = fixture.read(&["devbox"], &[]);
    assert!(!ok);
    assert_eq!(doc["error"]["code"], "ssh_failed", "{doc}");
    assert!(!doc.to_string().contains("192.0.2.1"), "{doc}");
}

/// Debug builds only: the deadline knob is ignored in release, where the read has its full 20 s.
#[cfg(debug_assertions)]
#[test]
fn a_remote_that_floods_or_hangs_is_cut_off() {
    for (name, body, text) in [
        ("flood", "cat >/dev/null; exec yes", "printed too much"),
        ("hang", "cat >/dev/null; exec sleep 60", "took too long"),
    ] {
        let fixture = Fixture::new(name);
        write_exec(
            &fixture.dir.join("bin/ssh"),
            &format!("#!/bin/sh\ncase \" $* \" in *\" -O check \"*) echo 'Master running (pid=1)' >&2; exit 0 ;; *\" -N \"*) exit 0 ;; esac\n{body}\n"),
        );
        let start = std::time::Instant::now();
        let (ok, doc) = fixture.read(&["devbox"], &[("HERDR_PORTAL_HISTORY_DEADLINE_MS", "1000")]);
        assert!(!ok, "{name}");
        assert!(
            doc["error"]["message"].as_str().unwrap().contains(text),
            "{name}: {doc}"
        );
        assert!(
            start.elapsed() < std::time::Duration::from_secs(10),
            "{name}"
        );
    }
}
