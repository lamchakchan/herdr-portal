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

/// The JSON shape's version: Headway accepts only schemas it knows.
pub(crate) const SCHEMA: u32 = 1;
/// Bytes kept of one file; a longer one is `too_large`.
const FILE_CAP: usize = 256 * 1024;
const PROGRESS_CAP: usize = 500;
const TEAMS_CAP: usize = 100;
const TRANSCRIPTS_CAP: usize = 500;

const PROGRESS_KEYS: [&str; 6] = [
    "pane_id",
    "terminal_id",
    "percent",
    "activity",
    "reported_at",
    "session_started_at",
];
const MEMBER_KEYS: [&str; 3] = ["name", "agentType", "joinedAt"];

pub(crate) const PROGRESS_FAILED: &str = "The progress read on the machine failed.";

/// Runs on the machine as `bash -s`. It prints one framed stream: `NOW\t<epoch s>`; per file `PROGRESS\t<name>` or
/// `TEAM\t<dir>`, its first 256 KiB + 1 byte as base64 lines, then `END` (never a base64 line, whose length is a
/// multiple of 4); per teammate transcript `TRANSCRIPT\t<lead id>\t<name>\t<mtime s>`; and `CAP\t<what>` when a cap
/// stopped it. A lead id reaches a path only after it matches `^[0-9a-f-]{36}$`. A missing folder prints nothing.
pub(crate) const SCRIPT: &str = r#"
set -u
cap=262144
total=0
printf 'NOW\t%s\n' "$(date +%s)"
emit() {
  size=$(wc -c < "$3" 2>/dev/null | tr -cd '0-9')
  size=${size:-0}
  [ "$size" -gt "$cap" ] && size=$((cap + 1))
  total=$((total + size))
  if [ "$total" -gt 2097152 ]; then printf 'CAP\ttotal\n'; exit 0; fi
  printf '%s\t%s\n' "$1" "$2"
  head -c $((cap + 1)) "$3" 2>/dev/null | base64
  printf 'END\n'
}
n=0
for f in "$HOME"/.herdr-projects/.progress/*.json; do
  [ -f "$f" ] || continue
  n=$((n + 1)); if [ "$n" -gt 500 ]; then printf 'CAP\tprogress\n'; break; fi
  emit PROGRESS "${f##*/}" "$f"
done
n=0
for f in "$HOME"/.claude/teams/*/config.json; do
  [ -f "$f" ] || continue
  n=$((n + 1)); if [ "$n" -gt 100 ]; then printf 'CAP\tteams\n'; break; fi
  d=${f%/config.json}
  emit TEAM "${d##*/}" "$f"
done
n=0
for f in "$HOME"/.claude/teams/*/config.json; do
  [ -f "$f" ] && head -c $((cap + 1)) "$f"
done 2>/dev/null | grep -o '"leadSessionId"[[:space:]]*:[[:space:]]*"[0-9a-f-]*"' | sed 's/.*"\([0-9a-f-]*\)"$/\1/' | grep -E '^[0-9a-f-]{36}$' | sort -u > "${TMPDIR:-/tmp}/hp-leads.$$"
while read -r id; do
  for f in "$HOME"/.claude/projects/*/"$id"/subagents/agent-a*.jsonl; do
    [ -f "$f" ] || continue
    n=$((n + 1)); if [ "$n" -gt 500 ]; then printf 'CAP\ttranscripts\n'; break 2; fi
    t=$(stat -c %.3Y "$f" 2>/dev/null || stat -f %Fm "$f" 2>/dev/null || stat -c %Y "$f" 2>/dev/null) || continue
    printf 'TRANSCRIPT\t%s\t%s\t%s\n' "$id" "${f##*/}" "$t"
  done
done < "${TMPDIR:-/tmp}/hp-leads.$$"
rm -f "${TMPDIR:-/tmp}/hp-leads.$$"
"#;

/// A base name the output may carry: plain characters only (no path, no control or separator characters).
fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 160
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

/// Standard base64, padding optional, white space ignored. None when it isn't base64.
fn decode_base64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for b in text.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b'\n' | b'\r' | b' ' | b'\t' => continue,
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

/// The keys asked for, from an object; everything else dropped.
fn only(value: &Value, keys: &[&str]) -> Option<Value> {
    let map = value.as_object()?;
    Some(Value::Object(
        keys.iter()
            .filter_map(|k| map.get(*k).map(|v| ((*k).to_string(), v.clone())))
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
    if let Some(lead) = map.get("leadSessionId") {
        config.insert("leadSessionId".into(), lead.clone());
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
    let mut transcripts: Vec<(String, Value)> = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let fields: Vec<&str> = line.split('\t').collect();
        match fields.as_slice() {
            ["NOW", secs] => now = secs.trim().parse::<i64>().ok(),
            ["CAP", _] => out.truncated = true,
            [kind @ ("PROGRESS" | "TEAM"), name] => {
                let mut body = String::new();
                let mut closed = false;
                for next in lines.by_ref() {
                    if next == "END" {
                        closed = true;
                        break;
                    }
                    body.push_str(next);
                }
                if !closed || !plain_name(name) {
                    continue;
                }
                let bytes = decode_base64(&body).unwrap_or_default();
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
                        Ok(config) => json!({"team": name, "config": config, "transcripts": []}),
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
                if transcripts.len() >= TRANSCRIPTS_CAP {
                    out.truncated = true;
                    continue;
                }
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
    }
    Ok(out)
}

/// The argv that runs `SCRIPT` on a remote machine: its SSH master, then `bash -s`, with the script on stdin.
pub(crate) fn remote_argv(machine: &Machine) -> Result<Vec<String>> {
    let mut args = ssh_base(&machine.target)?;
    args.extend([machine.target.clone(), "bash".into(), "-s".into()]);
    Ok(args)
}

/// Runs `SCRIPT` on the machine: (exit code, stdout, stderr).
fn run_script(machine: &Machine) -> Result<(i32, String, String)> {
    if is_local(machine) {
        return run_cmd(&["bash", "-s"], Some(SCRIPT));
    }
    ensure_master(&machine.target)?;
    let args = remote_argv(machine)?;
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run_cmd(&refs, Some(SCRIPT))
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
    // A fixed message: the selector could be an SSH target.
    let machine =
        resolve_machine(selector).map_err(|e| match e.downcast_ref::<api::ApiError>() {
            Some(api) if api.code == "machine_not_found" => err_with(
                "machine_not_found",
                "No saved machine matches the selector.",
                api.message.clone(),
            ),
            _ => e,
        })?;
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
