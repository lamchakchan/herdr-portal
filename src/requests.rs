//! Forwards that agents ask for. A pane on the remote machine sets
//! `portal_req_<port>` on itself; `watch` on the laptop opens the forward and
//! answers in `portal_st_<port>` on the same pane. See "Agent requests" in README.md.

use super::*;
use api::{err, err_with, parse_args, parse_port, print_json};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const REQ_SOURCE: &str = "herdr-portal";
const ST_SOURCE: &str = "herdr-portal-client";
const REQ_PREFIX: &str = "portal_req_";
const ST_PREFIX: &str = "portal_st_";
const MIN_PORT: u16 = 1024;
const AUTO_EVERY: Duration = Duration::from_secs(5);
const KNOWN_CODES: &[&str] = &[
    "invalid_args",
    "machine_not_found",
    "local_port_in_use",
    "no_forward",
    "not_inactive",
    "ssh_failed",
    "herdr_failed",
];

#[derive(Clone, Debug, PartialEq, Eq)]
enum Want {
    Same,
    Any,
    Port(u16),
}

impl Want {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "same" => Some(Want::Same),
            "any" => Some(Want::Any),
            other => other
                .parse::<u16>()
                .ok()
                .filter(|p| *p != 0)
                .map(Want::Port),
        }
    }

    fn as_token(&self) -> String {
        match self {
            Want::Same => "same".into(),
            Want::Any => "any".into(),
            Want::Port(port) => port.to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Status {
    Pending,
    Active(u16),
    Error(String),
}

impl Status {
    fn parse(text: &str) -> Option<Self> {
        match text.split_once(':') {
            None if text == "pending" => Some(Status::Pending),
            Some(("active", port)) => port.parse().ok().map(Status::Active),
            Some(("error", code)) if !code.is_empty() => Some(Status::Error(code.into())),
            _ => None,
        }
    }

    fn as_token(&self) -> String {
        match self {
            Status::Pending => "pending".into(),
            Status::Active(port) => format!("active:{port}"),
            Status::Error(code) => format!("error:{code}"),
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::Active(_) => "active",
            Status::Error(_) => "error",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Request {
    pane_id: String,
    tab_id: Option<String>,
    workspace_id: String,
    agent: Option<String>,
    remote_port: u16,
    want: Want,
    nonce: String,
    // The client's answer, only when it answers this request's nonce.
    status: Option<Status>,
    // The status token as found, to write only on change.
    status_raw: Option<String>,
}

// Nonces travel back through a remote shell, so only [0-9a-z] survive.
fn clean_nonce(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_ascii_digit() || c.is_ascii_lowercase())
        .take(32)
        .collect()
}

fn new_nonce() -> String {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    format!("{ms:x}{:x}", std::process::id())
}

fn split_nonce(value: &str) -> (&str, String) {
    match value.rsplit_once('@') {
        Some((body, nonce)) => (body, clean_nonce(nonce)),
        None => (value, String::new()),
    }
}

fn str_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn tokens(pane: &Value) -> Vec<(String, String)> {
    pane.get("tokens")
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// Requests a pane carries, with the client's answer when it matches.
fn pane_requests(pane: &Value) -> Vec<Request> {
    let Some(pane_id) = str_field(pane, "pane_id") else {
        return Vec::new();
    };
    let toks = tokens(pane);
    let mut out = Vec::new();
    for (key, value) in &toks {
        let Some(port) = key.strip_prefix(REQ_PREFIX) else {
            continue;
        };
        let Ok(remote_port) = port.parse::<u16>() else {
            continue;
        };
        let (body, nonce) = split_nonce(value);
        let Some(want) = Want::parse(body) else {
            continue;
        };
        let status_key = format!("{ST_PREFIX}{remote_port}");
        let status_raw = toks
            .iter()
            .find(|(k, _)| *k == status_key)
            .map(|(_, v)| v.clone());
        let status = status_raw.as_deref().and_then(|raw| {
            let (body, answered) = split_nonce(raw);
            (answered == nonce).then(|| Status::parse(body)).flatten()
        });
        out.push(Request {
            pane_id: pane_id.clone(),
            tab_id: str_field(pane, "tab_id"),
            workspace_id: str_field(pane, "workspace_id").unwrap_or_default(),
            agent: str_field(pane, "agent"),
            remote_port,
            want,
            nonce,
            status,
            status_raw,
        });
    }
    out.sort_by_key(|r| r.remote_port);
    out
}

/// Status tokens left on a pane whose request is gone.
fn orphan_statuses(pane: &Value) -> Vec<u16> {
    let toks = tokens(pane);
    toks.iter()
        .filter_map(|(k, _)| k.strip_prefix(ST_PREFIX)?.parse::<u16>().ok())
        .filter(|port| {
            let req = format!("{REQ_PREFIX}{port}");
            !toks.iter().any(|(k, _)| *k == req)
        })
        .collect()
}

fn panes_of(result: &Value) -> Vec<Value> {
    if let Some(rows) = result.get("panes").and_then(Value::as_array) {
        return rows.clone();
    }
    result.get("pane").cloned().into_iter().collect()
}

// `report-metadata` prints nothing on success, so only the exit code counts.
fn report_metadata(
    machine: &Machine,
    pane: &str,
    source: &str,
    flag: &str,
    arg: &str,
) -> Result<()> {
    let argv = herdr_argv(
        machine,
        &[
            "pane",
            "report-metadata",
            pane,
            "--source",
            source,
            flag,
            arg,
        ],
    );
    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let (code, stdout, stderr) = run_on(machine, &refs, None)?;
    if code != 0 {
        let detail = said(&stdout, &stderr);
        return Err(if is_local(machine) {
            err_with("herdr_failed", HERDR_FAILED, detail)
        } else {
            remote_failure(code, "herdr_failed", HERDR_FAILED, detail)
        });
    }
    Ok(())
}

fn set_token(machine: &Machine, pane: &str, source: &str, key: &str, value: &str) -> Result<()> {
    report_metadata(machine, pane, source, "--token", &format!("{key}={value}"))
}

fn clear_token(machine: &Machine, pane: &str, source: &str, key: &str) -> Result<()> {
    report_metadata(machine, pane, source, "--clear-token", key)
}

pub(crate) fn error_code(error: &anyhow::Error) -> String {
    api::error_code(error).to_string()
}

fn static_code(code: &str) -> &'static str {
    KNOWN_CODES
        .iter()
        .find(|known| **known == code)
        .copied()
        .unwrap_or("internal")
}

fn own_pane(args: &api::Args) -> Result<String> {
    args.flags
        .get("--pane")
        .cloned()
        .or_else(|| env::var("HERDR_PANE_ID").ok().filter(|p| !p.is_empty()))
        .ok_or_else(|| err("invalid_args", "Run inside a Herdr pane, or pass --pane ID"))
}

fn workspace_label(machine: &Machine, workspace_id: &str) -> String {
    load_workspaces(machine)
        .ok()
        .and_then(|rows| {
            rows.into_iter()
                .find(|(id, _)| id == workspace_id)
                .map(|(_, label)| label)
        })
        .unwrap_or_else(|| workspace_id.to_string())
}

fn request_json(req: &Request, label: Option<&str>) -> Value {
    let (status, local_port, error) = match &req.status {
        None | Some(Status::Pending) => ("pending", None, None),
        Some(Status::Active(port)) => ("active", Some(*port), None),
        Some(Status::Error(code)) => ("error", None, Some(code.clone())),
    };
    json!({
        "remote_port": req.remote_port,
        "requested": req.want.as_token(),
        "status": status,
        "local_port": local_port,
        "url": local_port.zip(label).map(|(p, l)| workspace_url(l, p)),
        "localhost_url": local_port.map(|p| format!("http://localhost:{p}")),
        "error": error,
        "pane_id": req.pane_id,
        "tab_id": req.tab_id,
        "workspace_id": (!req.workspace_id.is_empty()).then_some(&req.workspace_id),
        "agent": req.agent,
    })
}

fn read_request(machine: &Machine, pane: &str, port: u16) -> Result<Option<Request>> {
    let result = herdr_result(machine, &["pane", "get", pane])?;
    Ok(panes_of(&result)
        .iter()
        .flat_map(pane_requests)
        .find(|r| r.remote_port == port))
}

pub(crate) fn cmd_request(args: &[String], json_out: bool) -> Result<()> {
    let args = parse_args(args, &["--local-port", "--pane", "--wait"], &["--any"])?;
    let [port] = args.positional.as_slice() else {
        return Err(err(
            "invalid_args",
            "usage: herdr-portal request <port> [--local-port N | --any] [--pane ID] [--wait SECS] [--json]",
        ));
    };
    let remote_port = parse_port(port)?;
    if remote_port < MIN_PORT {
        return Err(err(
            "invalid_args",
            format!("Ports below {MIN_PORT} are not forwarded"),
        ));
    }
    let want = match (
        args.flags.get("--local-port"),
        args.switches.contains("--any"),
    ) {
        (Some(_), true) => return Err(err("invalid_args", "Pass --local-port or --any, not both")),
        (Some(local), false) => Want::Port(parse_port(local)?),
        (None, true) => Want::Any,
        (None, false) => Want::Same,
    };
    let wait = args
        .flags
        .get("--wait")
        .map(|s| {
            s.parse::<u64>()
                .map_err(|_| err("invalid_args", format!("'{s}' is not a number of seconds")))
        })
        .transpose()?
        .unwrap_or(0);
    let pane = own_pane(&args)?;
    let local = local_machine();
    let nonce = new_nonce();
    let key = format!("{REQ_PREFIX}{remote_port}");
    set_token(
        &local,
        &pane,
        REQ_SOURCE,
        &key,
        &format!("{}@{nonce}", want.as_token()),
    )?;

    let deadline = Instant::now() + Duration::from_secs(wait);
    let req = loop {
        let req = read_request(&local, &pane, remote_port)?
            .ok_or_else(|| anyhow!("Request token vanished from pane {pane}"))?;
        let answered = matches!(req.status, Some(Status::Active(_) | Status::Error(_)));
        if answered || Instant::now() >= deadline {
            break req;
        }
        std::thread::sleep(Duration::from_millis(500));
    };
    if let Some(Status::Error(code)) = &req.status {
        return Err(err(
            static_code(code),
            format!("The client could not forward port {remote_port}: {code}"),
        ));
    }
    let label = workspace_label(&local, &req.workspace_id);
    let row = request_json(&req, Some(&label));
    if json_out {
        return print_json(&json!({"ok": true, "request": row}));
    }
    match &req.status {
        Some(Status::Active(local_port)) => println!(
            "Forwarded {remote_port} -> {} on the laptop",
            workspace_url(&label, *local_port)
        ),
        _ => println!("Requested port {remote_port}; waiting for the laptop's watch"),
    }
    Ok(())
}

pub(crate) fn cmd_release(args: &[String], json_out: bool) -> Result<()> {
    let args = parse_args(args, &["--pane"], &[])?;
    let [port] = args.positional.as_slice() else {
        return Err(err(
            "invalid_args",
            "usage: herdr-portal release <port> [--pane ID] [--json]",
        ));
    };
    let remote_port = parse_port(port)?;
    let pane = own_pane(&args)?;
    clear_token(
        &local_machine(),
        &pane,
        REQ_SOURCE,
        &format!("{REQ_PREFIX}{remote_port}"),
    )?;
    if json_out {
        return print_json(&json!({"ok": true, "request": {
            "remote_port": remote_port,
            "pane_id": pane,
            "status": "released",
        }}));
    }
    println!("Released port {remote_port}; the laptop closes it on its next tick");
    Ok(())
}

pub(crate) fn cmd_requests(args: &[String], json_out: bool) -> Result<()> {
    let args = parse_args(args, &["--pane"], &["--all"])?;
    if !args.positional.is_empty() {
        return Err(err(
            "invalid_args",
            "usage: herdr-portal requests [--all | --pane ID] [--json]",
        ));
    }
    let local = local_machine();
    let result = if args.switches.contains("--all") {
        herdr_result(&local, &["pane", "list"])?
    } else {
        let pane = own_pane(&args)?;
        herdr_result(&local, &["pane", "get", &pane])?
    };
    let reqs: Vec<Request> = panes_of(&result).iter().flat_map(pane_requests).collect();
    let labels = load_workspaces(&local).unwrap_or_default();
    let label_of = |id: &str| {
        labels
            .iter()
            .find(|(wid, _)| wid == id)
            .map_or(id.to_string(), |(_, l)| l.clone())
    };
    if json_out {
        let rows: Vec<Value> = reqs
            .iter()
            .map(|r| request_json(r, Some(&label_of(&r.workspace_id))))
            .collect();
        return print_json(&json!({"ok": true, "requests": rows}));
    }
    if reqs.is_empty() {
        println!("No port requests.");
        return Ok(());
    }
    println!(
        "{:<10}{:<8}{:<10}{:<10}LOCAL",
        "PANE", "PORT", "ASKED", "STATUS"
    );
    for r in &reqs {
        let local = match &r.status {
            Some(Status::Active(p)) => p.to_string(),
            Some(Status::Error(code)) => code.clone(),
            _ => "-".into(),
        };
        println!(
            "{:<10}{:<8}{:<10}{:<10}{local}",
            r.pane_id,
            r.remote_port,
            r.want.as_token(),
            r.status.as_ref().map_or("pending", Status::name),
        );
    }
    Ok(())
}

fn free_local_port() -> Result<u16> {
    Ok(TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port())
}

pub(crate) fn same_machine(forward: &Forward, machine: &Machine) -> bool {
    forward.machine_id == machine.id
        || (!machine.target.is_empty() && forward.target == machine.target)
}

pub(crate) fn mark_origin(machine: &Machine, local_port: u16, origin: Origin) -> Result<()> {
    let mut items = load_forwards()?;
    for item in items.iter_mut() {
        if same_machine(item, machine) && item.local_port == local_port {
            item.origin = origin;
        }
    }
    save_forwards(&items)
}

/// Opens, resumes or reuses the forward one request asks for.
fn satisfy(
    machine: &Machine,
    req: &Request,
    labels: &[(String, String)],
    done: &HashMap<u16, u16>,
) -> (Status, &'static str) {
    if req.remote_port < MIN_PORT {
        return (Status::Error("invalid_args".into()), "error");
    }
    if let Some(local) = done.get(&req.remote_port) {
        return (Status::Active(*local), "shared");
    }
    let attempt = || -> Result<(u16, &'static str)> {
        if let Some(existing) = existing_forward(machine, req.remote_port)? {
            if forward_alive(&existing)? {
                let how = if existing.origin == Origin::Manual {
                    "adopt"
                } else {
                    "keep"
                };
                return Ok((existing.local_port, how));
            }
            restore_one(&existing)?;
            return Ok((existing.local_port, "resume"));
        }
        let local_port = match req.want {
            Want::Same => req.remote_port,
            Want::Port(port) => port,
            Want::Any if local_port_holder(req.remote_port).is_none() => req.remote_port,
            Want::Any => free_local_port()?,
        };
        let label = labels
            .iter()
            .find(|(id, _)| *id == req.workspace_id)
            .map_or(req.workspace_id.as_str(), |(_, l)| l.as_str());
        let forward = add_forward(
            machine,
            req.remote_port,
            Some(local_port),
            label,
            Some(&req.pane_id),
        )?;
        mark_origin(machine, forward.local_port, Origin::Request)?;
        Ok((forward.local_port, "add"))
    };
    match attempt() {
        Ok((local, how)) => (Status::Active(local), how),
        Err(error) => (Status::Error(error_code(&error)), "error"),
    }
}

fn tick_machine(machine: &Machine) -> Result<(Vec<Value>, HashSet<u16>)> {
    let result = herdr_result(machine, &["pane", "list"])?;
    let panes = panes_of(&result);
    let labels = load_workspaces(machine).unwrap_or_default();
    let mut actions = Vec::new();
    let mut done: HashMap<u16, u16> = HashMap::new();
    let mut wanted: HashSet<u16> = HashSet::new();

    for req in panes.iter().flat_map(pane_requests) {
        wanted.insert(req.remote_port);
        let (status, how) = satisfy(machine, &req, &labels, &done);
        if let Status::Active(local) = status {
            done.insert(req.remote_port, local);
        }
        let token = format!("{}@{}", status.as_token(), req.nonce);
        let wrote = if req.status_raw.as_deref() != Some(token.as_str()) {
            let key = format!("{ST_PREFIX}{}", req.remote_port);
            set_token(machine, &req.pane_id, ST_SOURCE, &key, &token).is_ok()
        } else {
            false
        };
        let answered = Request {
            status: Some(status),
            ..req.clone()
        };
        let label = labels
            .iter()
            .find(|(id, _)| *id == req.workspace_id)
            .map_or(req.workspace_id.as_str(), |(_, l)| l.as_str());
        let mut row = request_json(&answered, Some(label));
        let opened = how == "add" || how == "resume";
        row["action"] = json!(if opened || how == "error" || wrote {
            how
        } else {
            "none"
        });
        row["changed"] = json!(opened || wrote);
        actions.push(row);
    }

    for forward in load_forwards()? {
        if same_machine(&forward, machine)
            && forward.origin == Origin::Request
            && !wanted.contains(&forward.remote_port)
        {
            let outcome = stop_forward(forward.local_port, false);
            actions.push(json!({
                "action": if outcome.is_ok() { "stop" } else { "error" },
                "changed": outcome.is_ok(),
                "remote_port": forward.remote_port,
                "local_port": forward.local_port,
                "pane_id": forward.pane_id,
                "error": outcome.err().map(|e| error_code(&e)),
            }));
        }
    }

    for pane in &panes {
        let Some(pane_id) = str_field(pane, "pane_id") else {
            continue;
        };
        for port in orphan_statuses(pane) {
            let _ = clear_token(machine, &pane_id, ST_SOURCE, &format!("{ST_PREFIX}{port}"));
        }
    }
    Ok((actions, wanted))
}

/// One pass over every saved machine: agent requests always, auto-forward
/// when `with_auto`, closing auto forwards the `[auto]` rules no longer allow.
/// What one pass did. `auto` is None when the pass skipped auto-forward, and
/// otherwise holds the config error, if the config didn't load.
/// `machines` is what `--json` prints; `details` holds, per row, the full error text for watch.log and the plain CLI,
/// which can name the SSH target and so never goes in the JSON.
struct Pass {
    machines: Vec<Value>,
    details: Vec<RowDetail>,
    auto: Option<Option<Said>>,
}

#[derive(Default)]
struct RowDetail {
    error: Option<String>,
    auto_error: Option<String>,
}

/// An error as `--json` shows it, and as a person reads it.
struct Said {
    json: String,
    detail: String,
}

impl Said {
    fn of(error: &anyhow::Error) -> Said {
        Said {
            json: api::json_message(error),
            detail: format!("{error:#}"),
        }
    }
}

fn tick(with_auto: bool) -> Result<Pass> {
    let config = with_auto.then(auto::load_config);
    let config_error = match &config {
        Some(Err(error)) => Some(Some(Said::of(error))),
        Some(Ok(_)) => Some(None),
        None => None,
    };
    let mut machines = Vec::new();
    let mut details = Vec::new();
    for machine in load_machines()? {
        let mut row = json!({"machine": machine.label, "machine_id": machine.id});
        let mut detail = RowDetail::default();
        match tick_machine(&machine) {
            Ok((mut actions, wanted)) => {
                row["ok"] = json!(true);
                // Disabled still runs, so auto forwards close once auto is turned off.
                // A config that doesn't load runs nothing, leaving auto forwards as they are.
                if let Some(Ok(config)) = &config {
                    match auto::tick_machine(&machine, config, &wanted) {
                        Ok(more) => {
                            actions.extend(more);
                            row["auto_error"] = Value::Null;
                        }
                        Err(error) => {
                            row["auto_error"] = json!({
                                "code": error_code(&error),
                                "message": api::json_message(&error),
                            });
                            detail.auto_error = Some(format!("{error:#}"));
                        }
                    }
                }
                row["actions"] = json!(actions);
            }
            Err(error) => {
                row["ok"] = json!(false);
                row["error"] =
                    json!({"code": error_code(&error), "message": api::json_message(&error)});
                detail.error = Some(format!("{error:#}"));
            }
        }
        machines.push(row);
        details.push(detail);
    }
    Ok(Pass {
        machines,
        details,
        auto: config_error,
    })
}

fn stamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
        % 86_400;
    format!("{:02}:{:02}:{:02}Z", secs / 3600, secs / 60 % 60, secs % 60)
}

fn action_line(label: &str, a: &Value) -> String {
    let mut line = format!(
        "{label}: {} {} -> {} ({})",
        a["action"].as_str().unwrap_or("?"),
        a["remote_port"],
        a["local_port"],
        a["status"].as_str().unwrap_or("-"),
    );
    if let Some(code) = a["error"].as_str() {
        line.push_str(&format!(" {code}"));
    }
    line
}

fn print_actions(pass: &Pass) {
    for (m, detail) in pass.machines.iter().zip(&pass.details) {
        let label = m["machine"].as_str().unwrap_or("?");
        if m["ok"] == json!(false) {
            println!("{label}: {}", detail.error.as_deref().unwrap_or("error"));
            continue;
        }
        if let Some(message) = &detail.auto_error {
            println!("{label}: auto: {message}");
        }
        for a in m["actions"].as_array().into_iter().flatten() {
            println!("{}", action_line(label, a));
        }
    }
}

/// Prints a line the first time an error appears, changes or clears.
fn log_change(last: &mut HashMap<String, String>, key: String, message: String, what: &str) {
    if last.get(&key) == Some(&message) {
        return;
    }
    if message.is_empty() {
        if last.contains_key(&key) {
            println!("{} watch: {what}: ok again", stamp());
        }
        last.remove(&key);
    } else {
        println!("{} watch: {what}: {message}", stamp());
        last.insert(key, message);
    }
}

fn lock_path() -> Result<PathBuf> {
    Ok(state_dir()?.join("watch.lock"))
}

const LOG_FILE: &str = "watch.log";

fn log_path() -> Result<PathBuf> {
    Ok(state_dir()?.join(LOG_FILE))
}

/// Holds the single-instance lock for as long as the file stays open. The
/// descriptor is close-on-exec, so a re-exec hands the lock to the new image.
fn try_lock() -> Result<Option<fs::File>> {
    use std::os::unix::io::AsRawFd;
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path()?)?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Ok(None);
    }
    file.set_len(0)?;
    let mut writer = &file;
    writeln!(writer, "{}", std::process::id())?;
    Ok(Some(file))
}

fn lock_owner() -> Option<u32> {
    fs::read_to_string(lock_path().ok()?)
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// The binary herdr would run now, when this process runs from a plugin install.
fn installed_binary() -> Option<PathBuf> {
    let current = env::current_exe().ok()?.canonicalize().ok()?;
    let plugins = home_dir().ok()?.join(".config/herdr/plugins");
    if !current.starts_with(&plugins) {
        return None;
    }
    let text = fs::read_to_string(plugins.join("../plugins.json")).ok()?;
    let rows: Vec<Value> = serde_json::from_str(&text).ok()?;
    let root = rows
        .iter()
        .find(|row| row["plugin_id"] == json!(PLUGIN_ID))?["plugin_root"]
        .as_str()?
        .to_string();
    let candidate = Path::new(&root).join("herdr-portal").canonicalize().ok()?;
    (candidate != current).then_some(candidate)
}

fn reexec(binary: &Path) {
    use std::os::unix::process::CommandExt;
    let args: Vec<String> = env::args().skip(1).collect();
    println!("watch: plugin updated, switching to {}", binary.display());
    let error = Command::new(binary).args(&args).exec();
    println!("watch: re-exec failed: {error}; staying on this binary");
}

fn run_loop(interval: Duration, restore_first: bool) -> Result<()> {
    let Some(_lock) = try_lock()? else {
        println!(
            "watch: already running (pid {})",
            lock_owner().map_or("?".into(), |p| p.to_string())
        );
        return Ok(());
    };
    println!(
        "watch: started (pid {}), every {} ms",
        std::process::id(),
        interval.as_millis()
    );
    if restore_first {
        if let Err(error) = cmd_restore() {
            println!("watch: restore failed: {error:#}");
        }
    }
    let mut last_errors: HashMap<String, String> = HashMap::new();
    let mut last_auto: Option<Instant> = None;
    loop {
        if let Some(binary) = installed_binary() {
            reexec(&binary);
        }
        let with_auto = last_auto.is_none_or(|t| t.elapsed() >= AUTO_EVERY);
        if with_auto {
            last_auto = Some(Instant::now());
        }
        match tick(with_auto) {
            Ok(pass) => {
                for (m, detail) in pass.machines.iter().zip(&pass.details) {
                    let label = m["machine"].as_str().unwrap_or("?");
                    for a in m["actions"].as_array().into_iter().flatten() {
                        if a["changed"] == json!(true) {
                            println!("{} {}", stamp(), action_line(label, a));
                        }
                    }
                    let message = detail.error.clone().unwrap_or_default();
                    log_change(&mut last_errors, label.to_string(), message, label);
                    if with_auto && m["ok"] == json!(true) {
                        let message = detail.auto_error.clone().unwrap_or_default();
                        log_change(
                            &mut last_errors,
                            format!("{label} auto"),
                            message,
                            &format!("{label} auto"),
                        );
                    }
                }
                if let Some(config_error) = pass.auto {
                    log_change(
                        &mut last_errors,
                        "auto config".into(),
                        config_error.map(|said| said.detail).unwrap_or_default(),
                        "auto config",
                    );
                }
            }
            Err(error) => println!("{} watch: pass failed: {error:#}", stamp()),
        }
        let _ = io::stdout().flush();
        std::thread::sleep(interval);
    }
}

fn spawn_detached() -> Result<u32> {
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path()?)?;
    let mut cmd = Command::new(env::current_exe()?);
    cmd.args(["watch", "--restore-first"])
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let child = cmd.spawn().context("start watch")?;
    Ok(child.id())
}

/// The watch's state. `config` and `log` are file names, relative to the plugin's config and state folders, so the
/// JSON names no home path; the plain CLI prints the full log path from `log_path`.
fn watch_status() -> Result<Value> {
    let running = try_lock()?.is_none();
    let (enabled, error) = match auto::load_config() {
        Ok(config) => (config.enabled, None),
        Err(error) => (false, Some(api::json_message(&error))),
    };
    Ok(json!({
        "running": running,
        "pid": if running { lock_owner() } else { None },
        "log": LOG_FILE,
        "auto": {"enabled": enabled, "config": auto::CONFIG_FILE, "error": error},
    }))
}

pub(crate) fn cmd_start_watch(args: &[String], json_out: bool) -> Result<()> {
    if !args.is_empty() {
        return Err(err(
            "invalid_args",
            "usage: herdr-portal start-watch [--json]",
        ));
    }
    let status = watch_status()?;
    let (started, pid) = if status["running"] == json!(true) {
        (false, status["pid"].as_u64())
    } else {
        let pid = spawn_detached()?;
        std::thread::sleep(Duration::from_millis(300));
        (true, Some(u64::from(pid)))
    };
    if json_out {
        return print_json(&json!({"ok": true, "watch": {
            "started": started, "running": true, "pid": pid, "log": status["log"],
        }}));
    }
    match (started, pid) {
        (true, Some(pid)) => println!("Started watch (pid {pid}); log: {}", log_path()?.display()),
        (_, pid) => println!(
            "watch is already running (pid {})",
            pid.map_or("?".into(), |p| p.to_string())
        ),
    }
    Ok(())
}

pub(crate) fn cmd_watch(args: &[String], json_out: bool) -> Result<()> {
    let args = parse_args(
        args,
        &["--interval"],
        &["--once", "--restore-first", "--status"],
    )?;
    if !args.positional.is_empty() {
        return Err(err(
            "invalid_args",
            "usage: herdr-portal watch [--once | --status] [--restore-first] [--interval MS] [--json]",
        ));
    }
    if args.switches.contains("--status") {
        let status = watch_status()?;
        if json_out {
            return print_json(&json!({"ok": true, "watch": status}));
        }
        if status["running"] == json!(true) {
            println!("watch is running (pid {})", status["pid"]);
        } else {
            println!("watch is not running");
        }
        return Ok(());
    }
    if args.switches.contains("--once") {
        let pass = tick(true)?;
        let config_error = pass.auto.as_ref().and_then(Option::as_ref);
        if json_out {
            return print_json(&json!({
                "ok": true,
                "machines": pass.machines,
                "auto": {"error": config_error.map(|said| &said.json)},
            }));
        }
        if let Some(said) = config_error {
            println!("auto config: {}", said.detail);
        }
        print_actions(&pass);
        return Ok(());
    }
    let interval = match args.flags.get("--interval") {
        Some(ms) => ms
            .parse::<u64>()
            .ok()
            .filter(|ms| *ms >= 250)
            .map(Duration::from_millis)
            .ok_or_else(|| {
                err(
                    "invalid_args",
                    "--interval takes milliseconds, at least 250",
                )
            })?,
        None => Duration::from_secs(2),
    };
    run_loop(interval, args.switches.contains("--restore-first"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(tokens: Value) -> Value {
        json!({"pane_id": "w3:p2", "tab_id": "w3:t1", "workspace_id": "w3", "agent": "claude", "tokens": tokens})
    }

    #[test]
    fn want_round_trips() {
        for text in ["same", "any", "6000"] {
            assert_eq!(Want::parse(text).unwrap().as_token(), text);
        }
        assert!(Want::parse("0").is_none());
        assert!(Want::parse("bogus").is_none());
    }

    #[test]
    fn status_round_trips() {
        for text in ["pending", "active:5173", "error:local_port_in_use"] {
            assert_eq!(Status::parse(text).unwrap().as_token(), text);
        }
        assert!(Status::parse("active:x").is_none());
        assert!(Status::parse("error:").is_none());
        assert!(Status::parse("up").is_none());
    }

    #[test]
    fn nonce_keeps_only_safe_characters() {
        assert_eq!(clean_nonce("ab12;rm -rf"), "ab12rmrf");
        assert_eq!(split_nonce("same@Z9z$"), ("same", "9z".to_string()));
        assert_eq!(split_nonce("same"), ("same", String::new()));
        assert!(new_nonce().chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn request_reads_status_only_for_its_nonce() {
        let answered = pane(json!({
            "portal_req_5173": "same@abc",
            "portal_st_5173": "active:5173@abc",
            "hp_activity": "x",
        }));
        let reqs = pane_requests(&answered);
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].remote_port, 5173);
        assert_eq!(reqs[0].want, Want::Same);
        assert_eq!(reqs[0].status, Some(Status::Active(5173)));
        assert_eq!(reqs[0].agent.as_deref(), Some("claude"));

        let stale = pane(json!({
            "portal_req_5173": "any@new",
            "portal_st_5173": "active:5173@old",
        }));
        let reqs = pane_requests(&stale);
        assert_eq!(reqs[0].status, None);
        assert_eq!(reqs[0].status_raw.as_deref(), Some("active:5173@old"));
    }

    #[test]
    fn malformed_requests_are_ignored() {
        let p = pane(json!({
            "portal_req_x": "same@a",
            "portal_req_5173": "sometimes@a",
            "portal_req_8080": "8081@b",
        }));
        let reqs = pane_requests(&p);
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].want, Want::Port(8081));
    }

    #[test]
    fn orphan_statuses_are_found() {
        let p = pane(json!({
            "portal_req_5173": "same@a",
            "portal_st_5173": "active:5173@a",
            "portal_st_9000": "active:9000@b",
        }));
        assert_eq!(orphan_statuses(&p), vec![9000]);
        assert!(pane_requests(&json!({"tokens": {}})).is_empty());
    }

    #[test]
    fn request_json_reports_pending_active_and_error() {
        let mut req = pane_requests(&pane(json!({"portal_req_5173": "same@a"})))[0].clone();
        let row = request_json(&req, Some("web"));
        assert_eq!(row["status"], json!("pending"));
        assert_eq!(row["local_port"], Value::Null);
        req.status = Some(Status::Active(6000));
        let row = request_json(&req, Some("web"));
        assert_eq!(row["local_port"], json!(6000));
        assert_eq!(row["url"], json!("http://herdr.web.localhost:6000"));
        assert_eq!(row["localhost_url"], json!("http://localhost:6000"));
        req.status = Some(Status::Error("ssh_failed".into()));
        assert_eq!(request_json(&req, None)["error"], json!("ssh_failed"));
    }

    #[test]
    fn errors_are_logged_once_per_change() {
        let mut last = HashMap::new();
        log_change(&mut last, "k".into(), "bad".into(), "x");
        assert_eq!(last.get("k").map(String::as_str), Some("bad"));
        log_change(&mut last, "k".into(), "bad".into(), "x");
        assert_eq!(last.len(), 1);
        log_change(&mut last, "k".into(), String::new(), "x");
        assert!(last.is_empty());
    }

    #[test]
    fn skip_lines_carry_the_error_code() {
        let a = json!({"action": "auto-skip", "remote_port": 20487, "local_port": null,
                       "status": "skipped", "error": "local_port_in_use"});
        assert_eq!(
            action_line("dell", &a),
            "dell: auto-skip 20487 -> null (skipped) local_port_in_use"
        );
        assert!(stamp().ends_with('Z') && stamp().len() == 9);
    }

    #[test]
    fn unknown_client_codes_become_internal() {
        assert_eq!(static_code("local_port_in_use"), "local_port_in_use");
        assert_eq!(static_code("whatever"), "internal");
    }

    #[test]
    fn panes_of_accepts_list_and_get_results() {
        assert_eq!(
            panes_of(&json!({"panes": [{"pane_id": "a"}, {"pane_id": "b"}]})).len(),
            2
        );
        assert_eq!(panes_of(&json!({"pane": {"pane_id": "a"}})).len(), 1);
        assert!(panes_of(&json!({})).is_empty());
    }

    #[test]
    fn old_forward_state_loads_as_manual() {
        let old = r#"[{"machine_id":"m","label":"dell","target":"lam@h","remote_port":1,"local_port":1}]"#;
        let items: Vec<Forward> = serde_json::from_str(old).unwrap();
        assert_eq!(items[0].origin, Origin::Manual);
        let text = serde_json::to_string(&items[0]).unwrap();
        assert!(text.contains(r#""origin":"manual""#));
        let row = api::forward_json(&items[0], api::State::Inactive);
        assert_eq!(row["origin"], json!("manual"));
    }
}
