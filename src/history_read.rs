//! `herdr-portal history <Local|machine> --json [--since <epoch s>]` (rulings 1171, 1173, 1174): a machine's work
//! history, every line re-validated by `history::parse_line`, the writer's own schema. Local is read directly; another
//! machine through its SSH master with one fixed read-only script, which takes only a `yyyy-mm` month argument built
//! here from digits. The newest months are read first within READ_CAP bytes, and the newest EVENT_CAP events are kept.
//! `complete` is false whenever a line was dropped or anything was cut.

use super::*;
use crate::event::plain_number;
use crate::history::{month_file, parse_line, Event};
use crate::progress::{deadline_for, decode_base64, run_bounded, ReadTexts};
use api::{err, err_with, parse_args, print_json};
use std::io::{Read, Seek, SeekFrom};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) const SCHEMA: u32 = 1;
/// Bytes of history read per call, newest months first.
const READ_CAP: usize = 2 * 1024 * 1024;
/// Events kept per answer, newest first.
const EVENT_CAP: usize = 10_000;
/// The script's own output: READ_CAP in base64, plus its frames.
const OUTPUT_CAP: usize = 4 * 1024 * 1024;
const DAY: u64 = 86_400;
const DEFAULT_WINDOW: u64 = 7 * DAY;
const OLDEST_WINDOW: u64 = 400 * DAY;

pub(crate) const HISTORY_FAILED: &str = "The history read on the machine failed.";
const TEXTS: ReadTexts = ReadTexts {
    slow: "The history read on the machine took too long.",
    too_big: "The history read on the machine printed too much.",
};

/// Prints `NOW`, then for each month file at or after the month `$1`, newest first: `FILE <name> <bytes> <cut>`, the
/// file's bytes in base64, `END`. A file larger than what is left of the budget sends its newest bytes, cut=1, and
/// ends the read with `CAP`. It reads herdr-portal's default state directory, and writes nothing.
pub(crate) const SCRIPT: &str = r#"
set -u
export LC_ALL=C
budget=2097152
m=${1-}
case "$m" in [0-9][0-9][0-9][0-9]-[0-9][0-9]) ;; *) printf 'BAD\n'; exit 0 ;; esac
printf 'NOW\t%s\n' "$(date +%s)"
d="${XDG_STATE_HOME:-$HOME/.local/state}/herdr/plugins/lamchakchan.herdr-portal/history"
[ -d "$d" ] || exit 0
total=0
for f in $(ls -1 "$d" 2>/dev/null | grep -E '^[0-9]{4}-[0-9]{2}\.jsonl$' | sort -r); do
  [[ "${f%.jsonl}" < "$m" ]] && break
  [ -f "$d/$f" ] || continue
  size=$(wc -c < "$d/$f" 2>/dev/null | tr -cd '0-9')
  size=${size:-0}
  left=$((budget - total))
  if [ "$left" -le 0 ]; then printf 'CAP\tbytes\n'; break; fi
  if [ "$size" -gt "$left" ]; then
    printf 'FILE\t%s\t%s\t1\n' "$f" "$left"
    tail -c "$left" "$d/$f" 2>/dev/null | base64
    printf 'END\nCAP\tbytes\n'
    break
  fi
  printf 'FILE\t%s\t%s\t0\n' "$f" "$size"
  head -c "$size" "$d/$f" 2>/dev/null | base64
  printf 'END\n'
  total=$((total + size))
done
"#;

/// One month file as read: its bytes, and whether its front was cut off (so its first line is partial).
struct Chunk {
    bytes: Vec<u8>,
    cut: bool,
}

/// What a read found, before the lines are parsed.
struct Found {
    now: u64,
    /// Newest month first.
    chunks: Vec<Chunk>,
    capped: bool,
}

fn is_month_file(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() == 13
        && name.ends_with(".jsonl")
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
}

/// The month (`yyyy-mm`) to read from: since's, a day earlier, so a remote clock a little behind still finds it.
fn first_month(since: u64) -> String {
    month_file(since.saturating_sub(DAY))
        .trim_end_matches(".jsonl")
        .to_string()
}

/// This machine's history, read directly with the script's rules.
fn read_local(since: u64) -> Result<Found> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| err("internal", "The clock is before 1970."))?
        .as_secs();
    let dir = state_dir()?.join("history");
    let mut out = Found {
        now,
        chunks: Vec::new(),
        capped: false,
    };
    let Ok(entries) = fs::read_dir(&dir) else {
        return Ok(out);
    };
    let first = format!("{}.jsonl", first_month(since));
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| is_month_file(n) && *n >= first)
        .collect();
    names.sort_unstable_by(|a, b| b.cmp(a));
    let mut left = READ_CAP;
    for name in names {
        if left == 0 {
            out.capped = true;
            break;
        }
        let Ok(mut file) = fs::File::open(dir.join(&name)) else {
            continue;
        };
        let size = file.metadata().map(|m| m.len() as usize).unwrap_or(0);
        let cut = size > left;
        let take = size.min(left);
        if cut {
            file.seek(SeekFrom::Start((size - take) as u64))
                .map_err(|_| err("internal", HISTORY_FAILED))?;
        }
        let mut bytes = Vec::with_capacity(take);
        file.take(take as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| err("internal", HISTORY_FAILED))?;
        left -= bytes.len();
        out.chunks.push(Chunk { bytes, cut });
        if cut {
            out.capped = true;
            break;
        }
    }
    Ok(out)
}

/// Another machine's history, through its SSH master and SCRIPT.
fn read_remote(machine: &Machine, since: u64) -> Result<Found> {
    ensure_master(&machine.target)?;
    let month = first_month(since);
    debug_assert!(month.bytes().all(|b| b.is_ascii_digit() || b == b'-'));
    let mut args = ssh_base(&machine.target)?;
    // The month is digits and one hyphen (month_file), and the script checks it again.
    args.extend([
        machine.target.clone(),
        "bash".into(),
        "-s".into(),
        "--".into(),
        month,
    ]);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let (code, stdout, stderr) = run_bounded(
        &refs,
        SCRIPT,
        OUTPUT_CAP,
        deadline_for("HERDR_PORTAL_HISTORY_DEADLINE_MS"),
        &TEXTS,
    )?;
    if code != 0 {
        return Err(remote_failure(
            code,
            "internal",
            HISTORY_FAILED,
            stderr.trim(),
        ));
    }
    parse_stream(&stdout)
}

/// SCRIPT's output as chunks. A frame that doesn't parse fails the read: the script is fixed, so it means the machine
/// answered with something else.
fn parse_stream(text: &str) -> Result<Found> {
    let bad = || err_with("internal", HISTORY_FAILED, "unreadable history stream");
    let mut lines = text.lines();
    let mut out = Found {
        now: 0,
        chunks: Vec::new(),
        capped: false,
    };
    let mut saw_now = false;
    while let Some(line) = lines.next() {
        let fields: Vec<&str> = line.split('\t').collect();
        match fields.as_slice() {
            ["NOW", t] => {
                out.now = t.parse().map_err(|_| bad())?;
                saw_now = true;
            }
            ["FILE", name, _, cut] if is_month_file(name) && (*cut == "0" || *cut == "1") => {
                let mut body = String::new();
                loop {
                    match lines.next() {
                        Some("END") => break,
                        Some(l) => body.push_str(l),
                        None => return Err(bad()),
                    }
                }
                let bytes = decode_base64(&body).ok_or_else(bad)?;
                out.chunks.push(Chunk {
                    bytes,
                    cut: *cut == "1",
                });
            }
            ["CAP", "bytes"] => out.capped = true,
            _ => return Err(bad()),
        }
    }
    if !saw_now {
        return Err(bad());
    }
    Ok(out)
}

/// The events at or after `since`, oldest first, the newest EVENT_CAP of them, and whether nothing was lost.
fn events_of(read: Found, since: u64) -> (Vec<Event>, bool) {
    let mut complete = !read.capped;
    let mut events = Vec::new();
    for chunk in &read.chunks {
        // A byte that isn't UTF-8 becomes U+FFFD, which no field's pattern takes, so its line is dropped below.
        let text = String::from_utf8_lossy(&chunk.bytes);
        let mut body: &str = &text;
        if chunk.cut {
            // The front was cut: everything up to the first newline is part of a line.
            body = body.split_once('\n').map_or("", |(_, rest)| rest);
        }
        let mut segments: Vec<&str> = body.split('\n').collect();
        // A file ends with a newline, which leaves one empty segment; anything else there is a partial line.
        if segments.last() == Some(&"") {
            segments.pop();
        }
        for line in segments {
            match parse_line(line) {
                Some(e) => {
                    if e.at >= since {
                        events.push(e);
                    }
                }
                None => complete = false,
            }
        }
    }
    events.sort_by_key(|e| e.at);
    if events.len() > EVENT_CAP {
        events.drain(..events.len() - EVENT_CAP);
        complete = false;
    }
    (events, complete)
}

pub(crate) fn cmd_history(args: &[String], json_out: bool) -> Result<()> {
    const USAGE: &str = "usage: herdr-portal history <Local|machine> [--since <epoch s>] [--json]";
    let args = parse_args(args, &["--since"], &[])?;
    let [selector] = args.positional.as_slice() else {
        return Err(err("invalid_args", USAGE));
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| err("internal", "The clock is before 1970."))?
        .as_secs();
    let since = match args.flags.get("--since") {
        None => now.saturating_sub(DEFAULT_WINDOW),
        Some(v) => {
            let since =
                plain_number(v).ok_or_else(|| err("invalid_args", "--since is epoch seconds."))?;
            if since < now.saturating_sub(OLDEST_WINDOW) {
                return Err(err("invalid_args", "--since may be at most 400 days back."));
            }
            since
        }
    };
    // resolve_machine's errors name no selector in JSON (it could be an SSH target).
    let machine = resolve_machine(selector)?;
    let read = if is_local(&machine) {
        read_local(since)?
    } else {
        read_remote(&machine, since)?
    };
    let now = read.now;
    let (events, complete) = events_of(read, since);
    if json_out {
        return print_json(&json!({
            "ok": true,
            "schema": SCHEMA,
            "machine": machine.label,
            "now": now,
            "events": events,
            "complete": complete,
        }));
    }
    // Counts only, as for progress.
    println!(
        "{}: {} events{}",
        machine.label,
        events.len(),
        if complete { "" } else { " (incomplete)" }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_month_argument_is_digits_and_one_hyphen_a_day_before_since() {
        assert_eq!(first_month(1_796_083_200), "2026-11"); // 2026-12-01T00:00Z, less a day
        assert_eq!(first_month(1_796_169_600), "2026-12");
        assert_eq!(first_month(0), "1970-01");
        for since in [0, 1_600_000_000, 1_796_083_200, u64::from(u32::MAX)] {
            let m = first_month(since);
            assert!(
                m.len() == 7
                    && m.bytes().enumerate().all(|(i, b)| if i == 4 {
                        b == b'-'
                    } else {
                        b.is_ascii_digit()
                    }),
                "{m}"
            );
        }
    }

    #[test]
    fn the_script_refuses_any_month_argument_but_yyyy_mm() {
        for bad in [
            "",
            "2026-1",
            "2026-111",
            "2026_11",
            "$(id)",
            "../..",
            "2026-11\n",
        ] {
            let out = Command::new("bash")
                .args(["-s", "--", bad])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .and_then(|mut c| {
                    c.stdin.take().unwrap().write_all(SCRIPT.as_bytes())?;
                    c.wait_with_output()
                })
                .unwrap();
            assert_eq!(String::from_utf8_lossy(&out.stdout), "BAD\n", "{bad:?}");
        }
    }

    #[test]
    fn a_stream_frame_that_is_not_the_scripts_fails_the_read() {
        for text in [
            "",
            "NOW\tsoon\n",
            "NOW\t1\nFILE\t../x.jsonl\t1\t0\nAAAA\nEND\n",
            "NOW\t1\nFILE\t2026-11.jsonl\t1\t2\nAAAA\nEND\n",
            "NOW\t1\nFILE\t2026-11.jsonl\t1\t0\nAAAA\n",
            "NOW\t1\nFILE\t2026-11.jsonl\t1\t0\n!!!!\nEND\n",
            "NOW\t1\nBAD\n",
            "NOW\t1\nCAP\tother\n",
        ] {
            assert!(parse_stream(text).is_err(), "{text:?}");
        }
        let ok =
            parse_stream("NOW\t5\nFILE\t2026-11.jsonl\t2\t0\naGkK\nEND\nCAP\tbytes\n").unwrap();
        assert_eq!((ok.now, ok.capped, ok.chunks.len()), (5, true, 1));
        assert_eq!(ok.chunks[0].bytes, b"hi\n");
    }

    #[test]
    fn a_cut_chunk_drops_its_first_line_and_clears_complete() {
        let line = |w: &str| {
            format!(r#"{{"schema":1,"at":1796000000,"kind":"fix","work":"{w}","round":1}}"#)
        };
        let text = format!("{}\n{}\n", line("q-a"), line("q-b"));
        // Cut through the middle of the first line.
        let bytes = text.as_bytes()[10..].to_vec();
        let (events, complete) = events_of(
            Found {
                now: 0,
                chunks: vec![Chunk { bytes, cut: true }],
                capped: false,
            },
            0,
        );
        assert_eq!(
            events.iter().map(|e| e.work.as_str()).collect::<Vec<_>>(),
            ["q-b"]
        );
        assert!(complete, "a cut chunk alone is reported through `capped`");
        let (_, complete) = events_of(
            Found {
                now: 0,
                chunks: vec![Chunk {
                    bytes: text.into_bytes(),
                    cut: false,
                }],
                capped: true,
            },
            0,
        );
        assert!(!complete);
    }

    fn chunk(text: &[u8]) -> Found {
        Found {
            now: 0,
            chunks: vec![Chunk {
                bytes: text.to_vec(),
                cut: false,
            }],
            capped: false,
        }
    }

    /// An `at` inside the schema's range.
    const T: u64 = 1_796_000_000;

    fn fix(work: &str, at: u64) -> String {
        format!(r#"{{"schema":1,"at":{at},"kind":"fix","work":"{work}","round":1}}"#)
    }

    #[test]
    fn since_keeps_events_at_or_after_it_and_drops_older_ones_quietly() {
        let text = format!(
            "{}\n{}\n{}\n",
            fix("q-old", T - 1),
            fix("q-at", T),
            fix("q-new", T + 1)
        );
        let (events, complete) = events_of(chunk(text.as_bytes()), T);
        assert_eq!(
            events.iter().map(|e| e.work.as_str()).collect::<Vec<_>>(),
            ["q-at", "q-new"]
        );
        assert!(complete, "an event before since is filtered, not lost");
    }

    #[test]
    fn a_line_that_is_not_utf8_is_dropped_and_clears_complete() {
        let text = format!(
            "{}\n{}\n{}\n",
            fix("q-a", T),
            fix("q-b", T + 1),
            fix("q-c", T + 2)
        );
        let mut text = text.into_bytes();
        // The `b` of `q-b` becomes a byte that is not UTF-8.
        let at = text.windows(3).position(|w| w == b"q-b").unwrap() + 2;
        text[at] = 0xff;
        let (events, complete) = events_of(chunk(&text), 0);
        assert_eq!(
            events.iter().map(|e| e.work.as_str()).collect::<Vec<_>>(),
            ["q-a", "q-c"]
        );
        assert!(!complete);
    }
}
