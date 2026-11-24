//! `progress <machine> --json`: the herdr-projects progress records and the Claude Code team configs on a machine, for
//! Headway's agent rows (Headway rulings 1130, 1165a). One fixed script runs there per call, over the machine's SSH
//! master, or directly for Local. Nothing is interpolated into it.
//!
//! Privacy: a record keeps only `pane_id`, `terminal_id`, `percent`, `activity`, `reported_at` and
//! `session_started_at`. A team config keeps only `leadSessionId` and its members' `name`, `agentType` and
//! `joinedAt`. Every other key, a member's `prompt` first of all, is dropped before anything is printed. Teammate
//! transcripts are stat'd, never read. Nothing here is logged or written, and an error names no file content, no SSH
//! target and no stderr in `--json` mode.

use super::*;
use api::{err, err_with, parse_args, print_json};
use std::io::Read;
use std::time::{Duration, Instant};

/// The JSON shape's version: Headway accepts only schemas it knows.
pub(crate) const SCHEMA: u32 = 1;
/// Bytes kept of one file; a longer one is `too_large`.
const FILE_CAP: usize = 256 * 1024;
const PROGRESS_CAP: usize = 500;
const TEAMS_CAP: usize = 100;
/// Transcripts per lead (newest first), and in all.
const LEAD_TRANSCRIPTS_CAP: usize = 1000;
const TRANSCRIPTS_CAP: usize = 5000;
/// Characters kept of any string value.
const TEXT_CAP: usize = 256;
/// Longest file or folder name kept, the same in the script and here.
const NAME_CAP: usize = 160;
/// The read's whole output, and its deadline (ssh's ConnectTimeout does not cover a mux client).
const OUTPUT_CAP: usize = 4 * 1024 * 1024;
const DEADLINE: Duration = Duration::from_secs(20);

const PROGRESS_KEYS: [(&str, Kind); 6] = [
    ("pane_id", Kind::Text),
    ("terminal_id", Kind::Text),
    ("percent", Kind::Number),
    ("activity", Kind::Text),
    ("reported_at", Kind::Number),
    ("session_started_at", Kind::Number),
];
const MEMBER_KEYS: [(&str, Kind); 3] = [
    ("name", Kind::Text),
    ("agentType", Kind::Text),
    ("joinedAt", Kind::TextOrNumber),
];
/// Base64 text of one frame past which the body is no longer kept: FILE_CAP + 1 bytes, encoded, and some slack.
const BODY_CAP: usize = (FILE_CAP + 1).div_ceil(3) * 4 + 4096;

pub(crate) const PROGRESS_FAILED: &str = "The progress read on the machine failed.";
pub(crate) const PROGRESS_SLOW: &str = "The progress read on the machine took too long.";
pub(crate) const PROGRESS_TOO_BIG: &str = "The progress read on the machine printed too much.";

/// Runs on the machine as `bash -s`. It prints one framed stream: `NOW\t<epoch s>`; per file `PROGRESS\t<name>` or
/// `TEAM\t<dir>`, its first 256 KiB + 1 byte as base64 lines, then `END` (never a base64 line, whose length is a
/// multiple of 4); `TOO_LARGE\t<kind>\t<name>`, with no body, for a file past 256 KiB; per teammate transcript
/// `TRANSCRIPT\t<lead id>\t<name>\t<mtime s>`, newest first; `DONE\t<lead id>` once a lead's transcripts are all
/// listed; and `CAP\t<what>` when a cap stopped a section. Records and team configs each have a 1 MiB budget, so a
/// full records folder still leaves the teams. Transcripts: at most 1000 per lead and 5000 in all; a lead cut short
/// gets no `DONE`. A name longer than 160 bytes or with any character outside `[A-Za-z0-9._:-]` (a newline would
/// break the framing) is skipped. Lead ids come only from teams sent whole (only the config's top-level leadSessionId,
/// so a nested one is never read), and reach a path only after they match `^[0-9a-f-]{36}$`. A missing folder prints
/// nothing. It writes nothing on the machine.
pub(crate) const SCRIPT: &str = r#"
set -u
export LC_ALL=C
cap=262144
budget=1048576
tab=$(printf '\t')
nl='
'
printf 'NOW\t%s\n' "$(date +%s)"
# The top-level leadSessionId of the JSON file $1 (the last one, as the parser keeps), or nothing.
lead_of() {
  awk 'BEGIN { RS = "\001" } {
    n = length($0); d = 0; ins = 0; esc = 0; key = 0; want = 0; top = ""
    for (i = 1; i <= n; i++) {
      c = substr($0, i, 1)
      if (ins) {
        if (esc) esc = 0
        else if (c == "\\") esc = 1
        else if (c == "\"") {
          ins = 0; v = (i - b <= 64) ? substr($0, b, i - b) : ""
          if (want) { id = v; want = 0 } else key = (d == 1 && top == "{" && v == "leadSessionId")
        }
        continue
      }
      if (c == " " || c == "\t" || c == "\n" || c == "\r") continue
      if (c == "\"") { ins = 1; b = i + 1; continue }
      if (c == ":" && key) { want = 1; key = 0; continue }
      key = 0; want = 0
      if (c == "{" || c == "[") { if (d == 0 && top == "") top = c; d++ }
      else if (c == "}" || c == "]") d--
    }
  } END { if (id != "") print id }' "$1" 2>/dev/null
}
plain() { [ "${#1}" -le 160 ] || return 1; case "$1" in ''|*[!A-Za-z0-9._:-]*) return 1 ;; esac; }
emit() {
  size=$(wc -c < "$3" 2>/dev/null | tr -cd '0-9')
  size=${size:-0}
  if [ "$size" -gt "$cap" ]; then printf 'TOO_LARGE\t%s\t%s\n' "$1" "$2"; return 2; fi
  total=$((total + size))
  if [ "$total" -gt "$budget" ]; then printf 'CAP\t%s\n' "$1"; return 1; fi
  printf '%s\t%s\n' "$1" "$2"
  head -c $((cap + 1)) "$3" 2>/dev/null | base64
  printf 'END\n'
}
total=0; n=0
for f in "$HOME"/.herdr-projects/.progress/*.json; do
  [ -f "$f" ] || continue
  name=${f##*/}; plain "$name" || continue
  n=$((n + 1)); if [ "$n" -gt 500 ]; then printf 'CAP\tprogress\n'; break; fi
  emit PROGRESS "$name" "$f"; [ "$?" -eq 1 ] && break
done
total=0; n=0; ids=
for f in "$HOME"/.claude/teams/*/config.json; do
  [ -f "$f" ] || continue
  d=${f%/config.json}; name=${d##*/}; plain "$name" || continue
  n=$((n + 1)); if [ "$n" -gt 100 ]; then printf 'CAP\tteams\n'; break; fi
  emit TEAM "$name" "$f"; rc=$?
  [ "$rc" -eq 1 ] && break
  [ "$rc" -eq 0 ] || continue
  ids="$ids $(lead_of "$f")"
done
if stat -c %.3Y / 2>/dev/null | grep -Eq '^[0-9]+\.[0-9]+$'; then fmt=(-c "%.3Y${tab}%n")
elif stat -f %Fm / 2>/dev/null | grep -Eq '^[0-9]+(\.[0-9]+)?$'; then fmt=(-f "%Fm${tab}%N")
elif stat -c %Y / 2>/dev/null | grep -Eq '^[0-9]+$'; then fmt=(-c "%Y${tab}%n")
else fmt=(); fi
all=0
for id in $(for i in $ids; do printf '%s\n' "$i"; done | grep -E '^[0-9a-f-]{36}$' | sort -u); do
  set -- "$HOME"/.claude/projects/*/"$id"/subagents/agent-a*-????????????????.jsonl
  [ -e "$1" ] || { printf 'DONE\t%s\n' "$id"; continue; }
  files=()
  for f in "$@"; do
    case "$f" in *"$nl"*|*"$tab"*) continue ;; esac
    plain "${f##*/}" && files+=("$f")
  done
  k=0; cut=; stats=
  if [ "${#files[@]}" -gt 0 ]; then
    if [ "${#fmt[@]}" -eq 0 ]; then cut=1
    else stats=$(printf '%s\0' "${files[@]}" | xargs -0 -n 500 stat "${fmt[@]}" -- 2>/dev/null) || cut=1; fi
  fi
  while IFS="$tab" read -r t f; do
    case "$t" in ''|*[!0-9.]*|.*|*.|*.*.*) continue ;; esac
    name=${f##*/}; plain "$name" || continue
    k=$((k + 1)); if [ "$k" -gt 1000 ]; then printf 'CAP\ttranscripts\n'; cut=1; break; fi
    all=$((all + 1)); if [ "$all" -gt 5000 ]; then printf 'CAP\ttranscripts\n'; break 2; fi
    printf 'TRANSCRIPT\t%s\t%s\t%s\n' "$id" "$name" "$t"
  done < <(printf '%s\n' "$stats" | sort -t "$tab" -k1,1 -rn)
  [ -n "$cut" ] || printf 'DONE\t%s\n' "$id"
done
"#;

/// A base name the output may carry: plain characters only (no path, no control or separator characters).
fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= NAME_CAP
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
}

fn is_lead_id(id: &str) -> bool {
    id.len() == 36
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b) || b == b'-')
}

/// Standard base64, padding optional and only at the end, white space ignored. None when it isn't base64.
fn decode_base64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    let mut padded = false;
    for b in text.bytes() {
        if padded && !matches!(b, b'=' | b'\n' | b'\r' | b' ' | b'\t') {
            return None;
        }
        let v = match b {
            b'=' => {
                padded = true;
                continue;
            }
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'\n' | b'\r' | b' ' | b'\t' => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// The type a kept key's value must have. Anything else (an object or array above all, which could carry a dropped
/// key such as `prompt`) is dropped with the key.
#[derive(Clone, Copy)]
enum Kind {
    Text,
    Number,
    /// `joinedAt`: ms epoch, or an ISO string as some Claude Code versions write it.
    TextOrNumber,
}

impl Kind {
    /// The value to keep, or None. Text loses control and format characters and is cut to TEXT_CAP characters.
    /// Numbers pass unchanged: a caller clamps them.
    fn kept(self, value: &Value) -> Option<Value> {
        match (self, value) {
            (Kind::Text | Kind::TextOrNumber, Value::String(text)) => {
                Some(Value::String(clean_text(text)))
            }
            (Kind::Number | Kind::TextOrNumber, Value::Number(_)) => Some(value.clone()),
            _ => None,
        }
    }
}

/// A Unicode format character (category Cf): bidi overrides, zero-width marks and the like.
fn is_format(c: char) -> bool {
    matches!(u32::from(c),
        0xAD | 0x600..=0x605 | 0x61C | 0x6DD | 0x70F | 0x890..=0x891 | 0x8E2 | 0x180E | 0x200B..=0x200F
        | 0x202A..=0x202E | 0x2060..=0x2064 | 0x2066..=0x206F | 0xFEFF | 0xFFF9..=0xFFFB | 0x110BD | 0x110CD
        | 0x13430..=0x1343F | 0x1BCA0..=0x1BCA3 | 0x1D173..=0x1D17A | 0xE0001 | 0xE0020..=0xE007F)
}

/// Text as it may be printed: control (Cc) and format (Cf) characters removed, at most TEXT_CAP characters.
fn clean_text(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() && !is_format(*c))
        .take(TEXT_CAP)
        .collect()
}

/// The keys asked for, from an object, when their value has the kind asked for; everything else dropped.
fn only(value: &Value, keys: &[(&str, Kind)]) -> Option<Value> {
    let map = value.as_object()?;
    Some(Value::Object(
        keys.iter()
            .filter_map(|(k, kind)| {
                map.get(*k)
                    .and_then(|v| kind.kept(v))
                    .map(|v| ((*k).to_string(), v))
            })
            .collect(),
    ))
}

/// One progress file: its kept keys, or why it has none.
fn progress_entry(name: &str, bytes: &[u8]) -> Value {
    if bytes.len() > FILE_CAP {
        return json!({"file": name, "error": "too_large"});
    }
    match serde_json::from_slice::<Value>(bytes)
        .ok()
        .and_then(|doc| only(&doc, &PROGRESS_KEYS))
    {
        Some(record) => json!({"file": name, "record": record}),
        None => json!({"file": name, "error": "bad_json"}),
    }
}

/// One team config: `leadSessionId` and its members' kept keys, or why it has none.
fn team_config(bytes: &[u8]) -> Result<Value, &'static str> {
    if bytes.len() > FILE_CAP {
        return Err("too_large");
    }
    let doc: Value = serde_json::from_slice(bytes).map_err(|_| "bad_json")?;
    let map = doc.as_object().ok_or("bad_json")?;
    let mut config = serde_json::Map::new();
    if let Some(lead) = map.get("leadSessionId").and_then(|v| Kind::Text.kept(v)) {
        config.insert("leadSessionId".into(), lead);
    }
    if let Some(members) = map.get("members").and_then(Value::as_array) {
        config.insert(
            "members".into(),
            Value::Array(
                members
                    .iter()
                    .filter_map(|m| only(m, &MEMBER_KEYS))
                    .collect(),
            ),
        );
    }
    Ok(Value::Object(config))
}

/// What one read found, ready to print.
#[derive(Debug, Default)]
pub(crate) struct Collected {
    pub now: i64,
    pub progress: Vec<Value>,
    pub teams: Vec<Value>,
    pub truncated: bool,
}

/// The script's stream as JSON values. Unknown or malformed lines are skipped; a frame cut short is dropped.
pub(crate) fn parse_stream(text: &str) -> Result<Collected> {
    let mut out = Collected::default();
    let mut now = None;
    let mut clock_seen = false;
    let mut transcripts: Vec<(String, Value)> = Vec::new();
    let mut per_lead: HashMap<String, usize> = HashMap::new();
    let mut done: HashSet<String> = HashSet::new();
    let mut capped: HashSet<String> = HashSet::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let fields: Vec<&str> = line.split('\t').collect();
        match fields.as_slice() {
            // The first only, parsed or not: a later one could only come from a name that broke the framing.
            ["NOW", secs] if !clock_seen => {
                clock_seen = true;
                now = secs.trim().parse::<i64>().ok();
            }
            ["DONE", id] if is_lead_id(id) => {
                done.insert(id.to_string());
            }
            ["TOO_LARGE", kind @ ("PROGRESS" | "TEAM"), name] if plain_name(name) => {
                if *kind == "PROGRESS" {
                    if out.progress.len() >= PROGRESS_CAP {
                        out.truncated = true;
                        continue;
                    }
                    out.progress
                        .push(json!({"file": name, "error": "too_large"}));
                } else {
                    if out.teams.len() >= TEAMS_CAP {
                        out.truncated = true;
                        continue;
                    }
                    out.teams.push(json!({"team": name, "error": "too_large"}));
                }
            }
            ["CAP", _] => out.truncated = true,
            [kind @ ("PROGRESS" | "TEAM"), name] => {
                let mut body = String::new();
                let mut closed = false;
                let mut over = false;
                for next in lines.by_ref() {
                    if next == "END" {
                        closed = true;
                        break;
                    }
                    if body.len() + next.len() > BODY_CAP {
                        over = true;
                    } else {
                        body.push_str(next);
                    }
                }
                if !closed || !plain_name(name) {
                    continue;
                }
                let bytes = if over {
                    vec![0; FILE_CAP + 1]
                } else {
                    decode_base64(&body).unwrap_or_default()
                };
                if *kind == "PROGRESS" {
                    if out.progress.len() >= PROGRESS_CAP {
                        out.truncated = true;
                        continue;
                    }
                    out.progress.push(progress_entry(name, &bytes));
                } else {
                    if out.teams.len() >= TEAMS_CAP {
                        out.truncated = true;
                        continue;
                    }
                    out.teams.push(match team_config(&bytes) {
                        Ok(config) => json!({"team": name, "config": config, "transcripts": [], "transcripts_complete": false}),
                        Err(why) => json!({"team": name, "error": why}),
                    });
                }
            }
            ["TRANSCRIPT", id, name, mtime] => {
                if !is_lead_id(id) || !plain_name(name) {
                    continue;
                }
                let Ok(secs) = mtime.trim().parse::<f64>() else {
                    continue;
                };
                if !secs.is_finite() || secs < 0.0 {
                    continue;
                }
                let mine = per_lead.entry(id.to_string()).or_default();
                if transcripts.len() >= TRANSCRIPTS_CAP || *mine >= LEAD_TRANSCRIPTS_CAP {
                    out.truncated = true;
                    capped.insert(id.to_string());
                    continue;
                }
                *mine += 1;
                transcripts.push((
                    id.to_string(),
                    json!({"name": name, "mtime_ms": (secs * 1000.0).round() as i64}),
                ));
            }
            _ => {}
        }
    }
    out.now = now.ok_or_else(|| err("internal", "The machine's clock was not read."))?;
    for team in &mut out.teams {
        let lead = team["config"]["leadSessionId"].as_str().map(str::to_string);
        let Some(lead) = lead else { continue };
        let mine: Vec<Value> = transcripts
            .iter()
            .filter(|(id, _)| *id == lead)
            .map(|(_, t)| t.clone())
            .collect();
        team["transcripts"] = Value::Array(mine);
        // Complete only when the script listed every one of the lead's transcripts: a teammate missing from a cut list
        // has an unknown state, not "no transcript yet".
        team["transcripts_complete"] = json!(done.contains(&lead) && !capped.contains(&lead));
    }
    Ok(out)
}

/// The argv that runs `SCRIPT` on a remote machine: its SSH master, then `bash -s`, with the script on stdin.
pub(crate) fn remote_argv(machine: &Machine) -> Result<Vec<String>> {
    let mut args = ssh_base(&machine.target)?;
    args.extend([machine.target.clone(), "bash".into(), "-s".into()]);
    Ok(args)
}

/// Runs `SCRIPT` on the machine: (exit code, stdout, stderr), within OUTPUT_CAP and DEADLINE.
fn run_script(machine: &Machine) -> Result<(i32, String, String)> {
    if is_local(machine) {
        return run_bounded(&["bash", "-s"], SCRIPT, OUTPUT_CAP, deadline());
    }
    ensure_master(&machine.target)?;
    let args = remote_argv(machine)?;
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run_bounded(&refs, SCRIPT, OUTPUT_CAP, deadline())
}

/// DEADLINE, which a debug build lets tests shorten with `HERDR_PORTAL_PROGRESS_DEADLINE_MS`.
fn deadline() -> Duration {
    if cfg!(debug_assertions) {
        if let Some(ms) = std::env::var("HERDR_PORTAL_PROGRESS_DEADLINE_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
        {
            return Duration::from_millis(ms);
        }
    }
    DEADLINE
}

/// Runs `args` in a process group of its own, `input` on its stdin. Past `deadline`, or once stdout passes `limit`
/// bytes, the whole group is killed (ssh and anything the script started) and the read fails with fixed text. After
/// the child exits, the group is killed only if something it left behind still holds stdout open at the deadline:
/// then the group still has a member, so its id can't have been reused. stderr is kept up to 64 KiB.
fn run_bounded(
    args: &[&str],
    input: &str,
    limit: usize,
    deadline: Duration,
) -> Result<(i32, String, String)> {
    use std::os::unix::process::CommandExt;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    let mut child = Command::new(args[0])
        .args(&args[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| spawn_failure(args[0], &e))?;
    let group = child.id() as i32;
    let kill_group = || unsafe {
        libc::kill(-group, libc::SIGKILL);
    };
    let mut stdin = child.stdin.take().expect("piped stdin");
    let text = input.to_string();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(text.as_bytes());
    });
    let over = Arc::new(AtomicBool::new(false));
    let mut stdout = child.stdout.take().expect("piped stdout");
    let flag = Arc::clone(&over);
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = (&mut stdout).take(limit as u64 + 1).read_to_end(&mut buf);
        if buf.len() > limit {
            flag.store(true, Ordering::SeqCst);
        }
        buf
    });
    let mut stderr = child.stderr.take().expect("piped stderr");
    let errors = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = (&mut stderr).take(64 * 1024).read_to_end(&mut buf);
        let _ = io::copy(&mut stderr, &mut io::sink());
        buf
    });
    let start = Instant::now();
    let mut status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if over.load(Ordering::SeqCst) || start.elapsed() >= deadline {
            kill_group();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    while status.is_some() && !(reader.is_finished() && errors.is_finished()) {
        if over.load(Ordering::SeqCst) || start.elapsed() >= deadline {
            kill_group();
            status = None;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = writer.join();
    let out = reader.join().unwrap_or_default();
    let err_text = String::from_utf8_lossy(&errors.join().unwrap_or_default()).into_owned();
    match status {
        Some(_) if over.load(Ordering::SeqCst) => Err(err_with(
            "internal",
            PROGRESS_TOO_BIG,
            "output over the limit",
        )),
        Some(status) => Ok((
            status.code().unwrap_or(1),
            String::from_utf8_lossy(&out).into_owned(),
            err_text,
        )),
        None if over.load(Ordering::SeqCst) => Err(err_with(
            "internal",
            PROGRESS_TOO_BIG,
            "output over the limit",
        )),
        None => Err(err_with(
            "internal",
            PROGRESS_SLOW,
            format!("no answer within {} s", deadline.as_secs()),
        )),
    }
}

/// One read through `run` (the real one runs `SCRIPT`; tests pass a fake): the JSON `progress --json` prints.
pub(crate) fn collect(
    machine: &Machine,
    run: impl Fn(&Machine) -> Result<(i32, String, String)>,
) -> Result<Value> {
    let (code, stdout, stderr) = run(machine)?;
    if code != 0 {
        return Err(if is_local(machine) {
            err_with("internal", PROGRESS_FAILED, stderr.trim())
        } else {
            remote_failure(code, "internal", PROGRESS_FAILED, stderr.trim())
        });
    }
    let found = parse_stream(&stdout)?;
    Ok(json!({
        "ok": true,
        "schema": SCHEMA,
        "machine": machine.label,
        "machine_id": machine.id,
        "now": found.now,
        "progress": found.progress,
        "teams": found.teams,
        "truncated": found.truncated,
    }))
}

pub(crate) fn cmd_progress(args: &[String], json_out: bool) -> Result<()> {
    let args = parse_args(args, &[], &[])?;
    let [selector] = args.positional.as_slice() else {
        return Err(err(
            "invalid_args",
            "usage: herdr-portal progress <Local|machine> [--json]",
        ));
    };
    // resolve_machine's errors name no selector in JSON (it could be an SSH target).
    let machine = resolve_machine(selector)?;
    let out = collect(&machine, run_script)?;
    if json_out {
        return print_json(&out);
    }
    // Counts only: the values are for the JSON caller, never the terminal or a log.
    let count = |key: &str| out[key].as_array().map_or(0, Vec::len);
    println!(
        "{}: {} progress records, {} teams{}",
        machine.label,
        count("progress"),
        count("teams"),
        if out["truncated"] == json!(true) {
            " (capped)"
        } else {
            ""
        }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(bytes: &[u8]) -> String {
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let n =
                chunk.iter().fold(0u32, |a, b| (a << 8) | u32::from(*b)) << (8 * (3 - chunk.len()));
            for i in 0..4 {
                out.push(if i <= chunk.len() {
                    T[((n >> (18 - 6 * i)) & 63) as usize] as char
                } else {
                    '='
                });
            }
        }
        out
    }

    fn frame(kind: &str, name: &str, body: &[u8]) -> String {
        // GNU base64 wraps at 76 columns; the parser must not care.
        let text = b64(body);
        let wrapped: Vec<String> = text
            .as_bytes()
            .chunks(76)
            .map(|c| String::from_utf8_lossy(c).into_owned())
            .collect();
        format!("{kind}\t{name}\n{}\nEND\n", wrapped.join("\n"))
    }

    const LEAD: &str = "0f3c2a1e-5b6d-4e7f-8a9b-0c1d2e3f4a5b";

    #[test]
    fn base64_round_trips_and_rejects_other_text() {
        for text in [&b""[..], b"a", b"ab", b"abc", b"{\"pane_id\":\"w1:p2\"}"] {
            assert_eq!(decode_base64(&b64(text)).unwrap(), text);
        }
        assert!(decode_base64("not base64!").is_none());
    }

    #[test]
    fn a_progress_record_keeps_only_its_six_keys() {
        let record = br#"{"pane_id":"w1:p2","terminal_id":"t9","percent":55,"activity":"Waiting on CI","reported_at":1763999900,"session_started_at":1763990000,"prompt":"SENTINEL-PROMPT","cwd":"/home/someone/x"}"#;
        let found = parse_stream(&format!(
            "NOW\t1764000000\n{}",
            frame("PROGRESS", "w1_p2-1a2b3c4d.json", record)
        ))
        .unwrap();
        assert_eq!(found.now, 1764000000);
        let text = serde_json::to_string(&found.progress).unwrap();
        assert!(
            !text.contains("SENTINEL") && !text.contains("someone"),
            "{text}"
        );
        assert_eq!(
            found.progress[0]["record"]["activity"],
            json!("Waiting on CI")
        );
        assert_eq!(found.progress[0]["record"].as_object().unwrap().len(), 6);
    }

    #[test]
    fn a_team_keeps_its_lead_and_each_member_s_name_type_and_join_time_only() {
        let config = format!(
            r#"{{"leadSessionId":"{LEAD}","description":"SENTINEL-DESC","members":[{{"name":"impl-x","agentType":"general","joinedAt":1763990000000,"prompt":"SENTINEL-PROMPT","model":"m"}},"junk"]}}"#
        );
        let stream = format!(
            "NOW\t1764000000\n{}TRANSCRIPT\t{LEAD}\tagent-aimpl-x-0123456789abcdef.jsonl\t1763999950.25\nTRANSCRIPT\t{}\tagent-aother-0123456789abcdef.jsonl\t1\n",
            frame("TEAM", "squad", config.as_bytes()),
            "f".repeat(36)
        );
        let found = parse_stream(&stream).unwrap();
        let text = serde_json::to_string(&found.teams).unwrap();
        assert!(
            !text.contains("SENTINEL") && !text.contains("\"model\""),
            "{text}"
        );
        let team = &found.teams[0];
        assert_eq!(
            team["config"]["members"],
            json!([{"name": "impl-x", "agentType": "general", "joinedAt": 1763990000000_i64}])
        );
        assert_eq!(
            team["transcripts"],
            json!([{"name": "agent-aimpl-x-0123456789abcdef.jsonl", "mtime_ms": 1763999950250_i64}])
        );
    }

    #[test]
    fn bad_json_too_large_and_bad_names_are_marked_or_dropped() {
        let big = vec![b' '; FILE_CAP + 1];
        let stream = format!(
            "NOW\t5\n{}{}{}{}",
            frame("PROGRESS", "bad.json", b"{not json"),
            frame("PROGRESS", "big.json", &big),
            frame("PROGRESS", "../etc.json", b"{}"),
            frame("TEAM", "old", b"[1]")
        );
        let found = parse_stream(&stream).unwrap();
        assert_eq!(
            found.progress,
            vec![
                json!({"file": "bad.json", "error": "bad_json"}),
                json!({"file": "big.json", "error": "too_large"})
            ]
        );
        assert_eq!(
            found.teams,
            vec![json!({"team": "old", "error": "bad_json"})]
        );
    }

    #[test]
    fn a_cap_line_or_too_many_records_sets_truncated() {
        assert!(parse_stream("NOW\t5\nCAP\ttotal\n").unwrap().truncated);
        let many: String = (0..=PROGRESS_CAP)
            .map(|i| frame("PROGRESS", &format!("p{i}.json"), b"{}"))
            .collect();
        let found = parse_stream(&format!("NOW\t5\n{many}")).unwrap();
        assert_eq!(found.progress.len(), PROGRESS_CAP);
        assert!(found.truncated);
    }

    #[test]
    fn no_clock_is_an_internal_error_and_a_frame_cut_short_is_dropped() {
        assert!(parse_stream("PROGRESS\ta.json\n").is_err());
        let found = parse_stream("NOW\t5\nPROGRESS\ta.json\ne30=\n").unwrap();
        assert!(found.progress.is_empty());
    }

    #[test]
    fn a_kept_key_holding_an_object_or_the_wrong_type_is_dropped() {
        let record = br#"{"pane_id":{"prompt":"SENTINEL-PROMPT"},"terminal_id":"t","percent":"55","activity":["SENTINEL-PROMPT"],"reported_at":1,"session_started_at":1}"#;
        let config = br#"{"leadSessionId":{"prompt":"SENTINEL-PROMPT"},"members":[{"name":"m","agentType":{"prompt":"SENTINEL-PROMPT"},"joinedAt":{"t":1}}]}"#;
        let found = parse_stream(&format!(
            "NOW\t5\n{}{}",
            frame("PROGRESS", "p.json", record),
            frame("TEAM", "t", config)
        ))
        .unwrap();
        assert_eq!(
            found.progress[0]["record"],
            json!({"terminal_id": "t", "reported_at": 1, "session_started_at": 1})
        );
        assert_eq!(
            found.teams[0]["config"],
            json!({"members": [{"name": "m"}]})
        );
    }

    #[test]
    fn only_the_first_clock_counts_and_a_too_large_marker_has_no_body() {
        let found = parse_stream("NOW\t5\nNOW\t2\nTOO_LARGE\tPROGRESS\tbig.json\nTOO_LARGE\tTEAM\told\nTOO_LARGE\tTEAM\t../x\n").unwrap();
        assert_eq!(found.now, 5);
        assert_eq!(
            found.progress,
            vec![json!({"file": "big.json", "error": "too_large"})]
        );
        assert_eq!(
            found.teams,
            vec![json!({"team": "old", "error": "too_large"})]
        );
    }

    #[test]
    fn a_frame_body_past_the_cap_is_not_kept_in_memory() {
        let line = "A".repeat(76);
        let mut stream = String::from("NOW\t5\nPROGRESS\thuge.json\n");
        for _ in 0..(BODY_CAP / 76 + 10) {
            stream.push_str(&line);
            stream.push('\n');
        }
        stream.push_str("END\n");
        let found = parse_stream(&stream).unwrap();
        assert_eq!(
            found.progress,
            vec![json!({"file": "huge.json", "error": "too_large"})]
        );
    }

    #[test]
    fn padding_only_at_the_end() {
        assert!(decode_base64("YQ==YQ==").is_none());
        assert_eq!(decode_base64("YQ==\n").unwrap(), b"a");
    }

    #[test]
    fn a_join_time_may_be_iso_text_and_kept_text_loses_control_and_format_characters() {
        let config = "{\"leadSessionId\":\"0f3c2a1e-5b6d-4e7f-8a9b-0c1d2e3f4a5b\",\"members\":[{\"name\":\"a\\u202eb\\u009bc\\u200b\",\"agentType\":\"x\",\"joinedAt\":\"2026-11-24T10:00:00Z\"}]}";
        let record = format!(
            "{{\"pane_id\":\"w1:p1\",\"terminal_id\":\"t\",\"activity\":\"{}\"}}",
            "y".repeat(400)
        );
        let found = parse_stream(&format!(
            "NOW\t5\n{}{}",
            frame("TEAM", "t", config.as_bytes()),
            frame("PROGRESS", "p.json", record.as_bytes())
        ))
        .unwrap();
        assert_eq!(
            found.teams[0]["config"]["members"],
            json!([{"name": "abc", "agentType": "x", "joinedAt": "2026-11-24T10:00:00Z"}])
        );
        assert_eq!(
            found.progress[0]["record"]["activity"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            TEXT_CAP
        );
    }

    #[test]
    fn a_clock_that_does_not_parse_is_no_clock_even_with_a_later_one() {
        assert!(parse_stream("NOW\tsoon\nNOW\t5\n").is_err());
    }

    #[test]
    fn a_read_past_its_deadline_is_stopped_with_everything_it_started() {
        let dir = std::env::temp_dir().join(format!("hp-bounded-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let pidfile = dir.join("pid");
        let script = format!("sleep 30 & echo $! > '{}'; wait", pidfile.display());
        let start = Instant::now();
        let e =
            run_bounded(&["sh", "-c", &script], "", 1024, Duration::from_millis(500)).unwrap_err();
        assert!(start.elapsed() < Duration::from_secs(5));
        assert_eq!(api::json_message(&e), PROGRESS_SLOW);
        let pid: i32 = fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert_ne!(
            unsafe { libc::kill(pid, 0) },
            0,
            "the background sleep still runs"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_read_past_its_size_limit_is_stopped() {
        let start = Instant::now();
        let e =
            run_bounded(&["sh", "-c", "yes"], "", 64 * 1024, Duration::from_secs(20)).unwrap_err();
        assert!(start.elapsed() < Duration::from_secs(5));
        assert_eq!(api::json_message(&e), PROGRESS_TOO_BIG);
        let (code, out, _) =
            run_bounded(&["sh", "-c", "cat"], "hello", 1024, Duration::from_secs(5)).unwrap();
        assert_eq!((code, out.as_str()), (0, "hello"));
    }

    /// SCRIPT's raw stream for a HOME built by `setup`, run here with bash.
    fn raw_stream(name: &str, setup: impl Fn(&std::path::Path)) -> String {
        let home = std::env::temp_dir().join(format!("hp-script-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(&home).unwrap();
        setup(&home);
        let (code, out, err) = run_bounded_env(&["bash", "-s"], SCRIPT, &home);
        let _ = fs::remove_dir_all(&home);
        assert_eq!((code, err.as_str()), (0, ""));
        out
    }

    fn run_bounded_env(
        args: &[&str],
        input: &str,
        home: &std::path::Path,
    ) -> (i32, String, String) {
        let mut child = Command::new(args[0])
            .args(&args[1..])
            .env("HOME", home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        (
            out.status.code().unwrap_or(1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    #[test]
    fn the_script_and_the_parser_share_one_name_limit() {
        let out = raw_stream("names", |home| {
            let dir = home.join(".herdr-projects/.progress");
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join(format!("{}.json", "a".repeat(155))), "{}").unwrap();
            fs::write(dir.join(format!("{}.json", "b".repeat(156))), "{}").unwrap();
        });
        assert!(
            out.contains(&format!("PROGRESS\t{}.json", "a".repeat(155))),
            "{out}"
        );
        assert!(!out.contains(&"b".repeat(156)), "{out}");
        assert!(plain_name(&format!("{}.json", "a".repeat(155))));
        assert!(!plain_name(&format!("{}.json", "b".repeat(156))));
    }

    #[test]
    fn a_team_too_large_to_send_gives_no_lead_to_read() {
        let lead = "6a6b6c6d-0000-4000-8000-000000000006";
        let out = raw_stream("toolarge", |home| {
            let team = home.join(".claude/teams/big");
            fs::create_dir_all(&team).unwrap();
            let mut text = format!(r#"{{"leadSessionId":"{lead}","pad":""#);
            text.push_str(&"x".repeat(FILE_CAP + 10));
            text.push_str(r#""}"#);
            fs::write(team.join("config.json"), text).unwrap();
            let sub = home.join(format!(".claude/projects/p/{lead}/subagents"));
            fs::create_dir_all(&sub).unwrap();
            fs::write(sub.join("agent-ax-0123456789abcdef.jsonl"), "").unwrap();
        });
        assert!(out.contains("TOO_LARGE\tTEAM\tbig"), "{out}");
        assert!(!out.contains(lead), "{out}");
    }

    #[test]
    fn lead_ids_are_checked_before_they_reach_a_path() {
        assert!(is_lead_id(LEAD));
        assert!(!is_lead_id("../../../../etc/passwd-0000000000000000"));
        assert!(!is_lead_id(&format!("{}x", &LEAD[..35])));
        // The script itself filters with the same rule before building a path.
        assert!(SCRIPT.contains("grep -E '^[0-9a-f-]{36}$'"));
    }

    #[test]
    fn a_remote_read_is_ssh_bash_s_with_the_script_on_stdin_and_names_no_target_in_json() {
        let machine = Machine {
            id: "5b4e".into(),
            label: "devbox".into(),
            target: "someone@192.0.2.1".into(),
            session: String::new(),
        };
        let argv = remote_argv(&machine).unwrap();
        assert_eq!(&argv[argv.len() - 3..], ["someone@192.0.2.1", "bash", "-s"]);
        let ok = collect(&machine, |_| Ok((0, "NOW\t7\n".into(), String::new()))).unwrap();
        assert_eq!(ok["machine"], json!("devbox"));
        assert_eq!(ok["schema"], json!(1));
        assert!(!ok.to_string().contains("192.0.2.1"));
        for code in [255, 1] {
            let e = collect(&machine, |_| {
                Ok((
                    code,
                    String::new(),
                    "ssh: connect to host someone@192.0.2.1: refused".into(),
                ))
            })
            .unwrap_err();
            let out = api::error_json(&e).to_string();
            assert!(
                !out.contains("192.0.2.1") && !out.contains("someone"),
                "{out}"
            );
            assert!(
                out.contains(if code == 255 {
                    "ssh_failed"
                } else {
                    "internal"
                }),
                "{out}"
            );
        }
    }
}
