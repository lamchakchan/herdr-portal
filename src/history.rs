//! The work history's one schema (rulings 1171, 1173, 1174): `event` writes these lines and `history` reads them back,
//! both through `validate`, so the two can't drift. A line is one JSON object of fixed enums, ids and numbers; no field
//! holds free text. Each month's events go to `<state>/history/<yyyy-mm>.jsonl` (UTC).

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

pub(crate) const SCHEMA: u32 = 1;
/// A whole line, newline included, is at most this many bytes, so one write(2) appends it whole.
pub(crate) const LINE_CAP: usize = 1024;
/// `at` must fall in 2020-09-13 .. 2100-01-01: a clock or a line outside it is wrong.
const AT_MIN: u64 = 1_600_000_000;
const AT_MAX: u64 = 4_102_444_800;
const NUMBER_MAX: u64 = 1_000_000_000;
const ROUND_MAX: u64 = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Kind {
    Start,
    Agent,
    Review,
    Fix,
    Ci,
    Pr,
    End,
}

impl Kind {
    pub(crate) fn parse(text: &str) -> Option<Kind> {
        Some(match text {
            "start" => Kind::Start,
            "agent" => Kind::Agent,
            "review" => Kind::Review,
            "fix" => Kind::Fix,
            "ci" => Kind::Ci,
            "pr" => Kind::Pr,
            "end" => Kind::End,
            _ => return None,
        })
    }
}

/// One event. Every field but `schema`, `at`, `kind` and `work` belongs to some kinds only (`fields_of`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Event {
    pub(crate) schema: u32,
    pub(crate) at: u64,
    pub(crate) kind: Kind,
    pub(crate) work: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) terminal_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) verdict: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reviewer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) round: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) head: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) number: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) outcome: Option<String>,
}

impl Event {
    /// An event of `kind` for `work` at `at`, every optional field empty.
    pub(crate) fn new(kind: Kind, at: u64, work: &str) -> Event {
        Event {
            schema: SCHEMA,
            at,
            kind,
            work: work.to_string(),
            name: None,
            role: None,
            model: None,
            effort: None,
            pane_id: None,
            terminal_id: None,
            verdict: None,
            reviewer: None,
            round: None,
            result: None,
            head: None,
            repo: None,
            number: None,
            action: None,
            outcome: None,
        }
    }
}

/// Checks a whole event: schema, time, work id, which fields its kind allows and needs, and every value. The error is
/// the name of the first bad field (never its value), for a fixed message.
pub(crate) fn validate(event: &Event) -> Result<(), &'static str> {
    if event.schema != SCHEMA {
        return Err("schema");
    }
    if !(AT_MIN..=AT_MAX).contains(&event.at) {
        return Err("at");
    }
    if !work_id(&event.work) {
        return Err("work");
    }
    let (needs, allows) = fields_of(event.kind);
    for (field, present) in present(event) {
        if present != (needs.contains(&field) || (present && allows.contains(&field))) {
            return Err(field);
        }
    }
    let checks: [Check; 13] = [
        ("name", event.name.as_deref(), name),
        ("role", event.role.as_deref(), |v| ROLES.contains(&v)),
        ("model", event.model.as_deref(), model),
        ("effort", event.effort.as_deref(), |v| EFFORTS.contains(&v)),
        ("pane_id", event.pane_id.as_deref(), pane_id),
        ("terminal_id", event.terminal_id.as_deref(), name),
        ("verdict", event.verdict.as_deref(), |v| {
            VERDICTS.contains(&v)
        }),
        ("reviewer", event.reviewer.as_deref(), |v| {
            REVIEWERS.contains(&v)
        }),
        ("result", event.result.as_deref(), |v| RESULTS.contains(&v)),
        ("head", event.head.as_deref(), head),
        ("repo", event.repo.as_deref(), repo),
        ("action", event.action.as_deref(), |v| ACTIONS.contains(&v)),
        ("outcome", event.outcome.as_deref(), |v| {
            OUTCOMES.contains(&v)
        }),
    ];
    for (field, value, ok) in checks {
        if value.is_some_and(|v| !ok(v)) {
            return Err(field);
        }
    }
    if event.round.is_some_and(|n| !(1..=ROUND_MAX).contains(&n)) {
        return Err("round");
    }
    if event.number.is_some_and(|n| !(1..=NUMBER_MAX).contains(&n)) {
        return Err("number");
    }
    Ok(())
}

/// A line read back: at most LINE_CAP bytes, one JSON object with no unknown key, schema 1, and valid. None otherwise.
// The `history` reader (the next PR) is its caller; until then only the tests read lines back.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn parse_line(line: &str) -> Option<Event> {
    if line.len() + 1 > LINE_CAP {
        return None;
    }
    let event: Event = serde_json::from_str(line).ok()?;
    validate(&event).ok()?;
    Some(event)
}

/// The event as its line, newline included. The caller has validated it.
pub(crate) fn encode_line(event: &Event) -> String {
    let mut line = serde_json::to_string(event).expect("an event always serializes");
    line.push('\n');
    line
}

/// The month file an event at `at` (epoch s) goes to: `yyyy-mm.jsonl`, in UTC.
pub(crate) fn month_file(at: u64) -> String {
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm), for the year and month only.
    let z = (at / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}.jsonl")
}

const ROLES: [&str; 5] = ["builder", "reviewer", "designer", "fixer", "lead"];
const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
const VERDICTS: [&str; 3] = ["approve", "block", "changes"];
const REVIEWERS: [&str; 2] = ["self", "independent"];
const RESULTS: [&str; 2] = ["pass", "fail"];
const ACTIONS: [&str; 3] = ["opened", "merged", "closed"];
const OUTCOMES: [&str; 2] = ["done", "dropped"];

/// The fields a kind needs, and the optional ones it allows. Any other optional field must be absent.
fn fields_of(kind: Kind) -> (&'static [&'static str], &'static [&'static str]) {
    match kind {
        Kind::Start | Kind::Agent => (
            &["name", "role", "model"],
            &["effort", "pane_id", "terminal_id"],
        ),
        Kind::Review => (&["verdict", "reviewer", "name", "model"], &[]),
        Kind::Fix => (&["round"], &[]),
        Kind::Ci => (&["result", "head"], &[]),
        Kind::Pr => (&["repo", "number", "action"], &[]),
        Kind::End => (&["outcome"], &[]),
    }
}

/// Every optional field and whether it is set, in a fixed order.
fn present(e: &Event) -> [(&'static str, bool); 15] {
    [
        ("name", e.name.is_some()),
        ("role", e.role.is_some()),
        ("model", e.model.is_some()),
        ("effort", e.effort.is_some()),
        ("pane_id", e.pane_id.is_some()),
        ("terminal_id", e.terminal_id.is_some()),
        ("verdict", e.verdict.is_some()),
        ("reviewer", e.reviewer.is_some()),
        ("round", e.round.is_some()),
        ("result", e.result.is_some()),
        ("head", e.head.is_some()),
        ("repo", e.repo.is_some()),
        ("number", e.number.is_some()),
        ("action", e.action.is_some()),
        ("outcome", e.outcome.is_some()),
    ]
}

/// A string field: its name, its value, and the check its value must pass.
type Check<'a> = (&'static str, Option<&'a str>, fn(&str) -> bool);

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("a fixed pattern"))
}

/// A work id: a Jira key, `repo:branch` (`/` only between branch segments, none of them `.`, `..` or starting with
/// `-`), or a tracker id (lower case, may start with a digit: ruling 1174).
fn work_id(id: &str) -> bool {
    static JIRA: OnceLock<Regex> = OnceLock::new();
    static BRANCH: OnceLock<Regex> = OnceLock::new();
    static TRACKER: OnceLock<Regex> = OnceLock::new();
    if re(&JIRA, r"\A[A-Z][A-Z0-9]*-[1-9][0-9]{0,8}\z").is_match(id) {
        return true;
    }
    if id.len() <= 64 && re(&TRACKER, r"\A[a-z0-9]+(?:-[a-z0-9]+)*\z").is_match(id) {
        return true;
    }
    if id.len() > 128
        || !re(
            &BRANCH,
            r"\A[a-z0-9][a-z0-9._-]{0,63}:[A-Za-z0-9._-]+(?:/[A-Za-z0-9._-]+)*\z",
        )
        .is_match(id)
    {
        return false;
    }
    let branch = id.split_once(':').map_or("", |(_, b)| b);
    branch
        .split('/')
        .all(|seg| seg != "." && seg != ".." && !seg.starts_with('-'))
}

fn name(v: &str) -> bool {
    static P: OnceLock<Regex> = OnceLock::new();
    re(&P, r"\A[A-Za-z0-9][A-Za-z0-9._-]{0,63}\z").is_match(v)
}

fn model(v: &str) -> bool {
    static P: OnceLock<Regex> = OnceLock::new();
    re(&P, r"\A[a-z0-9][a-z0-9.-]{0,63}\z").is_match(v)
}

fn pane_id(v: &str) -> bool {
    static P: OnceLock<Regex> = OnceLock::new();
    re(&P, r"\A[A-Za-z0-9_-]{1,32}:[A-Za-z0-9_-]{1,32}\z").is_match(v)
}

fn head(v: &str) -> bool {
    static P: OnceLock<Regex> = OnceLock::new();
    re(&P, r"\A[0-9a-f]{7,40}\z").is_match(v)
}

fn repo(v: &str) -> bool {
    static P: OnceLock<Regex> = OnceLock::new();
    re(&P, r"\A[A-Za-z0-9-]{1,39}/[A-Za-z0-9._-]{1,100}\z").is_match(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    const AT: u64 = 1_795_550_000;

    fn start() -> Event {
        let mut e = Event::new(Kind::Start, AT, "AT-3400");
        e.name = Some("impl-filter".into());
        e.role = Some("builder".into());
        e.model = Some("claude-opus-5-5".into());
        e.effort = Some("high".into());
        e.pane_id = Some("w1:p2".into());
        e.terminal_id = Some("term-9".into());
        e
    }

    /// One valid event of every kind, with every field its kind allows.
    fn one_of_each() -> Vec<Event> {
        let mut agent = start();
        agent.kind = Kind::Agent;
        agent.role = Some("reviewer".into());
        let mut review = Event::new(Kind::Review, AT, "herdr-portal:feat/history-event");
        review.verdict = Some("changes".into());
        review.reviewer = Some("independent".into());
        review.name = Some("review-1".into());
        review.model = Some("opus".into());
        let mut fix = Event::new(Kind::Fix, AT, "1152");
        fix.round = Some(2);
        let mut ci = Event::new(Kind::Ci, AT, "AT-3400");
        ci.result = Some("fail".into());
        ci.head = Some("7ee0e8ad".into());
        let mut pr = Event::new(Kind::Pr, AT, "AT-3400");
        pr.repo = Some("glydways/headway".into());
        pr.number = Some(298);
        pr.action = Some("merged".into());
        let mut end = Event::new(Kind::End, AT, "q-sticky-agents");
        end.outcome = Some("done".into());
        vec![start(), agent, review, fix, ci, pr, end]
    }

    #[test]
    fn every_kind_round_trips_through_encode_and_parse() {
        for e in one_of_each() {
            assert_eq!(validate(&e), Ok(()), "{e:?}");
            let line = encode_line(&e);
            assert!(line.ends_with('\n') && line.len() <= LINE_CAP, "{line}");
            assert_eq!(line.matches('\n').count(), 1, "{line}");
            assert_eq!(parse_line(line.trim_end_matches('\n')), Some(e));
        }
        // The optional start fields may be left out.
        let mut bare = start();
        bare.effort = None;
        bare.pane_id = None;
        bare.terminal_id = None;
        assert_eq!(validate(&bare), Ok(()));
    }

    #[test]
    fn a_field_outside_its_kind_or_a_missing_one_is_refused() {
        type Set = fn(&mut Event);
        let extra: [(Kind, &str, Set); 7] = [
            (Kind::Start, "verdict", |e| {
                e.verdict = Some("approve".into())
            }),
            (Kind::Agent, "round", |e| e.round = Some(1)),
            (Kind::Review, "effort", |e| e.effort = Some("high".into())),
            (Kind::Fix, "name", |e| e.name = Some("x".into())),
            (Kind::Ci, "number", |e| e.number = Some(1)),
            (Kind::Pr, "head", |e| e.head = Some("abcdef0".into())),
            (Kind::End, "role", |e| e.role = Some("lead".into())),
        ];
        for (kind, field, set) in extra {
            let mut e = one_of_each().into_iter().find(|e| e.kind == kind).unwrap();
            set(&mut e);
            assert_eq!(validate(&e), Err(field), "{kind:?} with {field}");
        }
        let missing: [(Kind, &str, Set); 7] = [
            (Kind::Start, "model", |e| e.model = None),
            (Kind::Agent, "role", |e| e.role = None),
            (Kind::Review, "reviewer", |e| e.reviewer = None),
            (Kind::Fix, "round", |e| e.round = None),
            (Kind::Ci, "head", |e| e.head = None),
            (Kind::Pr, "action", |e| e.action = None),
            (Kind::End, "outcome", |e| e.outcome = None),
        ];
        for (kind, field, set) in missing {
            let mut e = one_of_each().into_iter().find(|e| e.kind == kind).unwrap();
            set(&mut e);
            assert_eq!(validate(&e), Err(field), "{kind:?} without {field}");
        }
    }

    /// (field, value, valid?) for the string fields, checked on the kind that carries each.
    const STRINGS: &[(&str, &str, bool)] = &[
        // work: a Jira key, repo:branch, or a tracker id (1174: may start with a digit).
        ("work", "AT-3400", true),
        ("work", "AB1-9", true),
        ("work", "AT-0", false),
        ("work", "AT-01", false),
        ("work", "at-3400", true), // a tracker id, not a key
        ("work", "AT-1234567890", false),
        ("work", "AT-", false),
        ("work", "xAT-1", false),
        ("work", "AT-1x", false),
        ("work", "herdr-portal:main", true),
        ("work", "herdr-portal:feat/history-event", true),
        ("work", "herdr-portal:feat/a.b_c-d/e", true),
        ("work", "herdr-portal:/feat", false),
        ("work", "herdr-portal:feat/", false),
        ("work", "herdr-portal:feat//x", false),
        ("work", "herdr-portal:feat/../x", false),
        ("work", "herdr-portal:..", false),
        ("work", "herdr-portal:./x", false),
        ("work", "herdr-portal:-x", false),
        ("work", "herdr-portal:feat/-x", false),
        ("work", "Herdr:main", false),
        ("work", ":main", false),
        ("work", "a:b:c", false),
        ("work", "1152", true),
        ("work", "0", true),
        ("work", "q-sticky-agents", true),
        ("work", "q--x", false),
        ("work", "q-", false),
        ("work", "-q", false),
        ("work", "q_x", false),
        ("work", "/x", false),
        ("work", "..", false),
        ("work", "", false),
        // name and terminal_id
        ("name", "impl-filter", true),
        ("name", "a", true),
        ("name", "A.b_c-9", true),
        ("name", "-x", false),
        ("name", ".x", false),
        ("name", "a b", false),
        ("name", "a/b", false),
        ("terminal_id", "term-9", true),
        ("terminal_id", "term:9", false),
        // model
        ("model", "claude-opus-5-5", true),
        ("model", "opus", true),
        ("model", "gpt.5", true),
        ("model", "Opus", false),
        ("model", "opus_5", false),
        ("model", "-opus", false),
        // enums
        ("role", "builder", true),
        ("role", "reviewer", true),
        ("role", "designer", true),
        ("role", "fixer", true),
        ("role", "lead", true),
        ("role", "Builder", false),
        ("role", "owner", false),
        ("effort", "low", true),
        ("effort", "medium", true),
        ("effort", "high", true),
        ("effort", "xhigh", true),
        ("effort", "max", true),
        ("effort", "ultra", false),
        ("verdict", "approve", true),
        ("verdict", "block", true),
        ("verdict", "changes", true),
        ("verdict", "merge", false),
        ("reviewer", "self", true),
        ("reviewer", "independent", true),
        ("reviewer", "peer", false),
        ("result", "pass", true),
        ("result", "fail", true),
        ("result", "ok", false),
        ("action", "opened", true),
        ("action", "merged", true),
        ("action", "closed", true),
        ("action", "reopened", false),
        ("outcome", "done", true),
        ("outcome", "dropped", true),
        ("outcome", "failed", false),
        // pane_id
        ("pane_id", "w1:p2", true),
        ("pane_id", "wE_x:p-9", true),
        ("pane_id", "w1p2", false),
        ("pane_id", "w1:p2:p3", false),
        ("pane_id", ":p2", false),
        // head: lowercase hex, 7..40
        ("head", "abcdef0", true),
        ("head", "7ee0e8ad7ee0e8ad7ee0e8ad7ee0e8ad7ee0e8ad", true),
        ("head", "abcdef", false),
        ("head", "7ee0e8ad7ee0e8ad7ee0e8ad7ee0e8ad7ee0e8ad0", false),
        ("head", "ABCDEF0", false),
        ("head", "abcdefg", false),
        // repo: owner/name
        ("repo", "glydways/headway", true),
        ("repo", "lamchakchan/herdr-portal", true),
        ("repo", "a/b.c_d-e", true),
        ("repo", "headway", false),
        ("repo", "a/b/c", false),
        ("repo", "a_b/c", false),
        ("repo", "/c", false),
        ("repo", "a/", false),
    ];

    /// The event of the kind that carries `field`, with `value` put in it.
    fn with(field: &str, value: &str) -> Event {
        let kind = match field {
            "verdict" | "reviewer" => Kind::Review,
            "result" | "head" => Kind::Ci,
            "repo" | "action" => Kind::Pr,
            "outcome" => Kind::End,
            _ => Kind::Start,
        };
        let mut e = one_of_each().into_iter().find(|e| e.kind == kind).unwrap();
        let v = Some(value.to_string());
        match field {
            "work" => e.work = value.to_string(),
            "name" => e.name = v,
            "role" => e.role = v,
            "model" => e.model = v,
            "effort" => e.effort = v,
            "pane_id" => e.pane_id = v,
            "terminal_id" => e.terminal_id = v,
            "verdict" => e.verdict = v,
            "reviewer" => e.reviewer = v,
            "result" => e.result = v,
            "head" => e.head = v,
            "repo" => e.repo = v,
            "action" => e.action = v,
            "outcome" => e.outcome = v,
            other => panic!("no field {other}"),
        }
        e
    }

    #[test]
    fn every_value_is_checked_against_its_pattern_or_enum() {
        for (field, value, ok) in STRINGS {
            let got = validate(&with(field, value));
            if *ok {
                assert_eq!(got, Ok(()), "{field}={value:?} should pass");
            } else {
                assert_eq!(got, Err(*field), "{field}={value:?} should fail");
            }
        }
        // Length bounds.
        assert_eq!(validate(&with("name", &"a".repeat(64))), Ok(()));
        assert_eq!(validate(&with("name", &"a".repeat(65))), Err("name"));
        assert_eq!(validate(&with("model", &"a".repeat(64))), Ok(()));
        assert_eq!(validate(&with("model", &"a".repeat(65))), Err("model"));
        assert_eq!(validate(&with("terminal_id", &"t".repeat(64))), Ok(()));
        assert_eq!(
            validate(&with("terminal_id", &"t".repeat(65))),
            Err("terminal_id")
        );
        assert_eq!(validate(&with("work", &"q".repeat(64))), Ok(()));
        assert_eq!(validate(&with("work", &"q".repeat(65))), Err("work"));
        let long_branch = format!("herdr-portal:{}", "b".repeat(128 - 13));
        assert_eq!(validate(&with("work", &long_branch)), Ok(()));
        assert_eq!(
            validate(&with("work", &format!("{long_branch}b"))),
            Err("work")
        );
        // Numbers.
        let mut fix = Event::new(Kind::Fix, AT, "AT-1");
        for (round, ok) in [(0, false), (1, true), (1000, true), (1001, false)] {
            fix.round = Some(round);
            assert_eq!(validate(&fix).is_ok(), ok, "round {round}");
        }
        let mut pr = with("repo", "a/b");
        for (number, ok) in [
            (0, false),
            (1, true),
            (1_000_000_000, true),
            (1_000_000_001, false),
        ] {
            pr.number = Some(number);
            assert_eq!(validate(&pr).is_ok(), ok, "number {number}");
        }
        for (at, ok) in [
            (1_599_999_999, false),
            (1_600_000_000, true),
            (4_102_444_800, true),
            (4_102_444_801, false),
        ] {
            let mut e = start();
            e.at = at;
            assert_eq!(validate(&e) == Err("at"), !ok, "at {at}");
        }
        let mut e = start();
        e.schema = 2;
        assert_eq!(validate(&e), Err("schema"));
    }

    #[test]
    fn injection_in_any_field_is_refused() {
        let fields = [
            "work",
            "name",
            "model",
            "terminal_id",
            "pane_id",
            "role",
            "effort",
            "verdict",
            "head",
            "repo",
        ];
        for field in fields {
            for bad in [
                "a\nb",
                "a\tb",
                "a\"b",
                "a\\b",
                "a\0b",
                "a/../b",
                "../a",
                "/a",
                "a//b",
                "a b",
                "é",
                "a\u{202e}b",
            ] {
                let value = match field {
                    // A value that is valid apart from the bad part.
                    "head" => format!("abcdef0{bad}"),
                    "pane_id" => format!("w1:p{bad}"),
                    "repo" => format!("x/y{bad}"),
                    _ => format!("x{bad}"),
                };
                assert_eq!(
                    validate(&with(field, &value)),
                    Err(field),
                    "{field}={value:?}"
                );
            }
        }
    }

    #[test]
    fn a_line_over_the_cap_or_with_unknown_keys_or_another_schema_is_not_parsed() {
        let good = encode_line(&start());
        let good = good.trim_end_matches('\n');
        assert!(parse_line(good).is_some());
        // Unknown key, wrong type, another schema, an invalid value, two objects, not an object.
        for bad in [
            good.replacen('{', r#"{"prompt":"x","#, 1),
            good.replacen(r#""schema":1"#, r#""schema":2"#, 1),
            good.replacen(r#""kind":"start""#, r#""kind":"merge""#, 1),
            good.replacen(r#""at":"#, r#""at":-"#, 1),
            good.replacen(r#""model":"claude-opus-5-5""#, r#""model":"x y""#, 1),
            good.replacen(r#""model":"claude-opus-5-5""#, r#""model":5"#, 1),
            good.replacen(
                r#""model":"claude-opus-5-5""#,
                r#""model":"claude-opus-5-5","model":"opus""#,
                1,
            ),
            format!("{good}{good}"),
            "[1]".into(),
            String::new(),
        ] {
            assert_eq!(parse_line(&bad), None, "{bad}");
        }
        // Past the cap, even when valid JSON with only known keys (white space padding).
        let padded = good.replacen('{', &format!("{{{}", " ".repeat(LINE_CAP)), 1);
        assert_eq!(parse_line(&padded), None);
        assert!(parse_line(&good.replacen(
            '{',
            &format!("{{{}", " ".repeat(LINE_CAP - 2 - good.len())),
            1
        ))
        .is_some());
    }

    #[test]
    fn the_longest_valid_event_of_each_kind_fits_one_write() {
        let long = |n: usize| "a".repeat(n);
        let work = format!("herdr-portal:{}", "b".repeat(115));
        let mut start = Event::new(Kind::Start, 4_102_444_800, &work);
        start.name = Some(long(64));
        start.role = Some("designer".into());
        start.model = Some(long(64));
        start.effort = Some("medium".into());
        start.pane_id = Some(format!("{}:{}", long(32), long(32)));
        start.terminal_id = Some(long(64));
        let mut review = Event::new(Kind::Review, 4_102_444_800, &work);
        review.verdict = Some("changes".into());
        review.reviewer = Some("independent".into());
        review.name = Some(long(64));
        review.model = Some(long(64));
        let mut pr = Event::new(Kind::Pr, 4_102_444_800, &work);
        pr.repo = Some(format!("{}/{}", long(39), long(100)));
        pr.number = Some(1_000_000_000);
        pr.action = Some("opened".into());
        for e in [start, review, pr] {
            assert_eq!(validate(&e), Ok(()), "{e:?}");
            let len = encode_line(&e).len();
            assert!(len <= LINE_CAP, "{len}");
        }
    }

    #[test]
    fn month_file_is_the_utc_month() {
        assert_eq!(month_file(1_796_083_199), "2026-11.jsonl"); // 2026-11-30T23:59:59Z
        assert_eq!(month_file(1_796_083_200), "2026-12.jsonl"); // 2026-12-01T00:00:00Z
        assert_eq!(month_file(1_798_761_599), "2026-12.jsonl"); // 2026-12-31T23:59:59Z
        assert_eq!(month_file(1_798_761_600), "2027-01.jsonl"); // 2027-01-01T00:00:00Z
        assert_eq!(month_file(1_709_164_800), "2024-02.jsonl"); // 2024-02-29T00:00:00Z, a leap day
        assert_eq!(month_file(1_709_251_199), "2024-02.jsonl"); // 2024-02-29T23:59:59Z
        assert_eq!(month_file(1_709_251_200), "2024-03.jsonl"); // 2024-03-01T00:00:00Z
        assert_eq!(month_file(1_600_000_000), "2020-09.jsonl");
    }
}
