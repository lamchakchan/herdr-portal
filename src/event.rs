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
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
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
    // A flag given twice would be last-wins; refuse it instead (1176).
    for flag in FLAGS {
        if args.iter().filter(|a| a.as_str() == flag).count() > 1 {
            return Err(invalid(&flag[2..]));
        }
    }
    let args = parse_args(args, &FLAGS, &[])?;
    let [kind] = args.positional.as_slice() else {
        return Err(err("invalid_args", USAGE));
    };
    let kind = Kind::parse(kind).ok_or_else(|| err("invalid_args", USAGE))?;
    let flag = |name: &str| args.flags.get(name).cloned();
    let number = |name: &str, field: &'static str| -> Result<Option<u64>> {
        flag(name)
            .map(|v| plain_number(&v).ok_or_else(|| invalid(field)))
            .transpose()
    };
    let work = flag("--work").ok_or_else(|| invalid("work"))?;
    let at = now()?;
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
    validate(&event).map_err(|field| {
        if field == "at" {
            // The portal set the time, so this is the machine's clock, not the caller's input.
            err("internal", "This machine's clock is outside 2020 to 2100.")
        } else {
            invalid(field)
        }
    })?;
    let line = encode_line(&event);
    // Every field is bounded, so a valid event fits; this stays a real check all the same (1176).
    line_fits(&line)?;
    append(&history_dir()?, &month_file(at), line.as_bytes())?;
    if json_out {
        return print_json(&json!({"ok": true, "event": event}));
    }
    Ok(())
}

/// The portal's clock in epoch seconds. A debug build lets tests set it with `HERDR_PORTAL_EVENT_NOW`, so a clock
/// outside 2020 to 2100 can be tested; release builds ignore it, and no flag sets the time.
fn now() -> Result<u64> {
    if cfg!(debug_assertions) {
        if let Some(at) = std::env::var("HERDR_PORTAL_EVENT_NOW")
            .ok()
            .and_then(|v| v.parse().ok())
        {
            return Ok(at);
        }
    }
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| err("internal", "The clock is before 1970."))?
        .as_secs())
}

/// Plain digits with no sign, space or leading zero (but `0` itself), at most 10 of them.
fn plain_number(text: &str) -> Option<u64> {
    let digits = !text.is_empty() && text.len() <= 10 && text.bytes().all(|b| b.is_ascii_digit());
    if !digits || (text.len() > 1 && text.starts_with('0')) {
        return None;
    }
    text.parse().ok()
}

/// `internal` when `line` is longer than one whole write may be.
fn line_fits(line: &str) -> Result<()> {
    if line.len() > LINE_CAP {
        return Err(err(
            "internal",
            "The event is longer than a history line may be.",
        ));
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

/// `<state>/history`, made 0700 (and set to 0700 if it was looser). A symlink or anything but a directory there is
/// refused before any chmod, so the chmod never reaches another directory (1176).
pub(crate) fn history_dir() -> Result<PathBuf> {
    let dir = state_dir()?.join("history");
    let refused = || {
        err(
            "internal",
            "The history directory is not a plain directory.",
        )
    };
    match fs::symlink_metadata(&dir) {
        Ok(meta) if !meta.file_type().is_dir() => return Err(refused()),
        Ok(_) => {}
        Err(_) => {
            if let Err(e) = fs::DirBuilder::new().mode(0o700).create(&dir) {
                if e.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(err("internal", "Could not make the history directory."));
                }
            }
            // Made by us or by a writer running at once: still only a plain directory.
            if !fs::symlink_metadata(&dir).is_ok_and(|m| m.file_type().is_dir()) {
                return Err(refused());
            }
        }
    }
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
        .map_err(|_| err("internal", "Could not make the history directory private."))?;
    Ok(dir)
}

/// Appends `line` with one write(2) on an O_APPEND fd, so concurrent writers never interleave. A short write fails and
/// leaves the file as it is (1174): truncating back could cut another writer's line, and the reader drops a partial one.
///
/// The file is opened without following a symlink and without blocking (a FIFO fails at once), then must be a regular
/// file this user owns, and is made 0600 through the open fd (1176).
fn append(dir: &std::path::Path, name: &str, line: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(dir.join(name))
        .map_err(|_| err("internal", "Could not open the history file."))?;
    let meta = file
        .metadata()
        .map_err(|_| err("internal", "Could not open the history file."))?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let me = unsafe { libc::geteuid() };
    if !meta.file_type().is_file() || meta.uid() != me {
        return Err(err(
            "internal",
            "The history file is not a plain file of this user's.",
        ));
    }
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|_| err("internal", "Could not make the history file private."))?;
    write_line(&mut file, line)
}

/// One write of the whole line; a short write fails and is never retried, which could split the line.
fn write_line(out: &mut impl Write, line: &[u8]) -> Result<()> {
    match out.write(line) {
        Ok(n) if n == line.len() => Ok(()),
        _ => Err(err("internal", "The history write was cut short.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A writer that takes one byte less than it is given.
    struct Short;
    impl Write for Short {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            Ok(buf.len().saturating_sub(1))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_short_write_fails_and_is_not_retried() {
        let e = write_line(&mut Short, b"{}\n").unwrap_err();
        assert_eq!(crate::api::error_code(&e), "internal");
        let mut whole = Vec::new();
        write_line(&mut whole, b"{}\n").unwrap();
        assert_eq!(whole, b"{}\n");
    }

    #[test]
    fn a_line_over_the_cap_is_internal_and_one_at_it_fits() {
        assert!(line_fits(&"a".repeat(LINE_CAP)).is_ok());
        let e = line_fits(&"a".repeat(LINE_CAP + 1)).unwrap_err();
        assert_eq!(crate::api::error_code(&e), "internal");
    }

    #[test]
    fn numbers_are_plain_digits_with_no_leading_zero() {
        for (text, ok) in [
            ("1", true),
            ("1000", true),
            ("0", true),
            ("01", false),
            ("+1", false),
            ("1 ", false),
            ("", false),
        ] {
            assert_eq!(plain_number(text).is_some(), ok, "{text:?}");
        }
    }
}
