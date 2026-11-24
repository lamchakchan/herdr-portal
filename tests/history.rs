//! `event` end to end (rulings 1171, 1173, 1174): the real binary on a temp HOME and state dir. A good event appends
//! one whole line at the portal's own time; a bad one writes nothing and never echoes its value; files are 0600 in a
//! 0700 directory; and 20 writers at once leave only whole lines.

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
        Fixture { dir }
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
        let _ = fs::remove_dir_all(&self.dir);
    }
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
        // 1176: an unknown --flag=value, a duplicate flag, leading zeros.
        vec![
            "event",
            "end",
            "--work",
            "AT-1",
            "--outcome",
            "done",
            "--prompt=ZZ-SECRET words",
        ],
        // 1178: an unknown flag whose name is not a plain name is not echoed at all.
        vec![
            "event",
            "end",
            "--work",
            "AT-1",
            "--outcome",
            "done",
            "--prompt ZZ-SECRET words",
        ],
        vec![
            "event",
            "end",
            "--work",
            "AT-1",
            "--outcome",
            "done",
            "--prompt:ZZ-SECRET",
        ],
        vec![
            "event",
            "end",
            "--work",
            "AT-1",
            "--work",
            "AT-2",
            "--outcome",
            "done",
        ],
        vec!["event", "fix", "--work", "AT-1", "--round", "0001"],
        vec![
            "event", "pr", "--work", "AT-1", "--repo", "a/b", "--number", "007", "--action",
            "opened",
        ],
    ]
    .iter()
    .map(|c| c.iter().map(|s| s.to_string()).collect())
    .collect();
    // 1176: a Jira-shaped id of 3,000 bytes is not a work id.
    let huge = format!("ZZSECRET{}-1", "A".repeat(3000));
    let mut cases = cases;
    cases.push(
        ["event", "end", "--work", huge.as_str(), "--outcome", "done"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
    );
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
fn a_month_file_that_is_a_link_a_fifo_or_loose_is_refused_or_tightened() {
    use std::os::unix::fs::symlink;
    let month = month_of(now());
    let end = [
        "event",
        "end",
        "--work",
        "AT-1",
        "--outcome",
        "done",
        "--json",
    ];
    // A symlink to a file elsewhere: refused, and the target untouched.
    let fixture = Fixture::new("link");
    fs::create_dir_all(fixture.history()).unwrap();
    let outside = fixture.dir.join("outside.txt");
    fs::write(&outside, "keep\n").unwrap();
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o644)).unwrap();
    symlink(&outside, fixture.history().join(&month)).unwrap();
    let out = fixture.run(&end);
    assert!(!out.status.success());
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["error"]["code"], "internal", "{doc}");
    assert_eq!(fs::read_to_string(&outside).unwrap(), "keep\n");
    assert_eq!(mode(&outside), 0o644);

    // A hard link to a file elsewhere (1178): refused, and the target untouched.
    let fixture = Fixture::new("hardlink");
    fs::create_dir_all(fixture.history()).unwrap();
    let outside = fixture.dir.join("outside.txt");
    fs::write(&outside, "keep\n").unwrap();
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o644)).unwrap();
    fs::hard_link(&outside, fixture.history().join(&month)).unwrap();
    let out = fixture.run(&end);
    assert!(!out.status.success());
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["error"]["code"], "internal", "{doc}");
    assert_eq!(fs::read_to_string(&outside).unwrap(), "keep\n");
    assert_eq!(mode(&outside), 0o644);

    // A dangling symlink: refused, and nothing is made where it points.
    let fixture = Fixture::new("dangling");
    fs::create_dir_all(fixture.history()).unwrap();
    let nowhere = fixture.dir.join("made-by-the-link.jsonl");
    symlink(&nowhere, fixture.history().join(&month)).unwrap();
    assert!(!fixture.run(&end).status.success());
    assert!(!nowhere.exists());

    // A FIFO: refused at once, never a hang.
    let fixture = Fixture::new("fifo");
    fs::create_dir_all(fixture.history()).unwrap();
    let made = Command::new("mkfifo")
        .arg(fixture.history().join(&month))
        .status()
        .unwrap();
    assert!(made.success());
    let start = std::time::Instant::now();
    let mut child = fixture.command(&end).spawn().unwrap();
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if start.elapsed() > std::time::Duration::from_secs(5) {
            let _ = child.kill();
            panic!("event hung on a FIFO");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    assert!(!status.success());

    // An existing file that is too loose is made 0600 through the open file.
    let fixture = Fixture::new("loose");
    fs::create_dir_all(fixture.history()).unwrap();
    let file = fixture.history().join(&month);
    fs::write(&file, "").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o666)).unwrap();
    assert!(fixture.run(&end).status.success());
    assert_eq!(mode(&file), 0o600);
}

#[test]
fn a_history_directory_that_is_a_link_or_a_file_is_refused_and_never_chmodded() {
    use std::os::unix::fs::symlink;
    let end = [
        "event",
        "end",
        "--work",
        "AT-1",
        "--outcome",
        "done",
        "--json",
    ];
    let fixture = Fixture::new("dirlink");
    let elsewhere = fixture.dir.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::set_permissions(&elsewhere, fs::Permissions::from_mode(0o755)).unwrap();
    symlink(&elsewhere, fixture.history()).unwrap();
    let out = fixture.run(&end);
    assert!(!out.status.success());
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["error"]["code"], "internal", "{doc}");
    assert_eq!(mode(&elsewhere), 0o755);
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);

    let fixture = Fixture::new("dirfile");
    fs::write(fixture.history(), "not a directory").unwrap();
    assert!(!fixture.run(&end).status.success());
    assert_eq!(
        fs::read_to_string(fixture.history()).unwrap(),
        "not a directory"
    );
}

#[test]
fn a_fifo_with_a_reader_is_still_refused_and_gets_nothing() {
    let fixture = Fixture::new("fifo-read");
    fs::create_dir_all(fixture.history()).unwrap();
    let fifo = fixture.history().join(month_of(now()));
    assert!(Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap()
        .success());
    // A reader holds the FIFO open, so the open itself succeeds; only the file-type check stops the write.
    let mut reader = Command::new("cat")
        .arg(&fifo)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));
    let out = fixture.run(&[
        "event",
        "end",
        "--work",
        "AT-1",
        "--outcome",
        "done",
        "--json",
    ]);
    assert!(!out.status.success(), "{out:?}");
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["error"]["code"], "internal", "{doc}");
    let _ = reader.kill();
    let got = reader.wait_with_output().unwrap();
    assert!(got.stdout.is_empty(), "the FIFO got a line");
}

/// Debug builds only: the clock knob is ignored in release.
#[cfg(debug_assertions)]
#[test]
fn a_clock_outside_2020_to_2100_is_internal_and_writes_nothing() {
    let fixture = Fixture::new("clock");
    for at in ["5", "4102444801"] {
        let out = fixture
            .command(&[
                "event",
                "end",
                "--work",
                "AT-1",
                "--outcome",
                "done",
                "--json",
            ])
            .env("HERDR_PORTAL_EVENT_NOW", at)
            .output()
            .unwrap();
        assert!(!out.status.success());
        let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(doc["error"]["code"], "internal", "{at}: {doc}");
    }
    assert_eq!(fixture.lines(), Vec::<String>::new());
}
