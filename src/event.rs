//! `herdr-portal event <kind> [--field value …]` (rulings 1171, 1173, 1174): appends one work-history line to
//! `<state>/history/<yyyy-mm>.jsonl`. The portal sets the time. Every value is checked by `history::validate`, and a
//! bad event fails with `invalid_args`, naming the field but never its value, and writes nothing.

use crate::api::{err, parse_args, print_json};
use crate::history::{encode_line, month_file, validate, Event, Kind, LINE_CAP};
use crate::state_dir;
use anyhow::Result;
use serde_json::json;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const USAGE: &str =
    "usage: herdr-portal event <start|agent|review|fix|ci|pr|end> --work <id> [--field value …]";

/// The flags `event` takes, each the field it sets. There is no flag for `at` or `schema`.
const FLAGS: [&str; 16] = [
    "--work",
    "--name",
    "--role",
    "--model",
    "--effort",
    "--pane",
    "--terminal",
    "--verdict",
    "--reviewer",
    "--round",
    "--result",
    "--head",
    "--repo",
    "--number",
    "--action",
    "--outcome",
];

pub(crate) fn cmd_event(args: &[String], json_out: bool) -> Result<()> {
    let args = parse_args(args, &FLAGS, &[])?;
    let [kind] = args.positional.as_slice() else {
        return Err(err("invalid_args", USAGE));
    };
    let kind = Kind::parse(kind).ok_or_else(|| err("invalid_args", USAGE))?;
    let flag = |name: &str| args.flags.get(name).cloned();
    let number = |name: &str, field: &'static str| -> Result<Option<u64>> {
        flag(name)
            .map(|v| {
                // Digits only: no sign, no space, nothing parse would forgive.
                if v.is_empty() || v.len() > 10 || !v.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(invalid(field));
                }
                v.parse().map_err(|_| invalid(field))
            })
            .transpose()
    };
    let work = flag("--work").ok_or_else(|| invalid("work"))?;
    let at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| err("internal", "The clock is before 1970."))?
        .as_secs();
    let mut event = Event::new(kind, at, &work);
    event.name = flag("--name");
    event.role = flag("--role");
    event.model = flag("--model");
    event.effort = flag("--effort");
    event.pane_id = flag("--pane");
    event.terminal_id = flag("--terminal");
    event.verdict = flag("--verdict");
    event.reviewer = flag("--reviewer");
    event.round = number("--round", "round")?;
    event.result = flag("--result");
    event.head = flag("--head");
    event.repo = flag("--repo");
    event.number = number("--number", "number")?;
    event.action = flag("--action");
    event.outcome = flag("--outcome");
    validate(&event).map_err(invalid)?;
    let line = encode_line(&event);
    // Every field is bounded, so a valid event always fits (history's longest-event test).
    debug_assert!(line.len() <= LINE_CAP);
    append(&history_dir()?, &month_file(at), line.as_bytes())?;
    if json_out {
        return print_json(&json!({"ok": true, "event": event}));
    }
    Ok(())
}

/// `invalid_args` naming the field, never its value.
fn invalid(field: &'static str) -> anyhow::Error {
    err(
        "invalid_args",
        format!("The event's {field} is missing, not allowed for this kind, or not a valid value."),
    )
}

/// `<state>/history`, made 0700 (and set to 0700 if it was looser).
pub(crate) fn history_dir() -> Result<PathBuf> {
    let dir = state_dir()?.join("history");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .map_err(|_| err("internal", "Could not make the history directory."))?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
        .map_err(|_| err("internal", "Could not make the history directory private."))?;
    Ok(dir)
}

/// Appends `line` with one write(2) on an O_APPEND fd, so concurrent writers never interleave. A short write fails and
/// leaves the file as it is (1174): truncating back could cut another writer's line, and the reader drops a partial one.
fn append(dir: &std::path::Path, name: &str, line: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(dir.join(name))
        .map_err(|_| err("internal", "Could not open the history file."))?;
    match file.write(line) {
        Ok(n) if n == line.len() => Ok(()),
        _ => Err(err("internal", "The history write was cut short.")),
    }
}
