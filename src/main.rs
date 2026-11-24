use anyhow::{anyhow, bail, Context, Result};
use regex::Regex;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::io::{self, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

mod api;
mod auto;
mod event;
mod history;
mod picker;
mod progress;
mod requests;
mod sharing;

const PLUGIN_ID: &str = "lamchakchan.herdr-portal";
const LOCAL_ID: &str = "local";

#[derive(Clone, Debug)]
struct Machine {
    id: String,
    label: String,
    target: String,
    session: String,
}

#[derive(Clone, Debug)]
struct Listener {
    port: u16,
    addr: String,
    process: String,
    pid: Option<u32>,
}

#[derive(Clone, Debug, Default)]
struct WorkspacePort {
    workspace_id: String,
    workspace_label: String,
    port: u16,
    process: String,
    // Set only when process ancestry ties the listener to a pane.
    tab_id: Option<String>,
    pane_id: Option<String>,
    agent: Option<String>,
    agent_name: Option<String>,
    pid: Option<u32>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Forward {
    machine_id: String,
    label: String,
    target: String,
    remote_port: u16,
    local_port: u16,
    #[serde(default)]
    workspace_label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pane_id: Option<String>,
    #[serde(default)]
    origin: Origin,
}

/// Who opened a forward. `watch` only ever stops the ones it opened.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum Origin {
    #[default]
    Manual,
    Request,
    Auto,
}

#[derive(Deserialize)]
struct MachineRow {
    id: String,
    label: String,
    target: String,
    #[serde(default)]
    session: String,
    #[serde(default = "default_true")]
    enabled: bool,
}

fn default_true() -> bool {
    true
}

fn main() {
    let mut args: Vec<String> = env::args().skip(1).collect();
    let json_out = args.iter().any(|a| a == "--json");
    args.retain(|a| a != "--json");
    if let Err(err) = run(args, json_out) {
        if json_out {
            println!("{}", api::error_json(&err));
        } else {
            eprintln!("{err:#}");
        }
        std::process::exit(1);
    }
}

/// Commands that print plain text only. With `--json` they fail with `invalid_args`, so a JSON caller never parses
/// text or starts a picker. Loop-mode `watch` is refused in `cmd_watch`.
const PLAIN_ONLY: [&str; 7] = [
    "pick",
    "restore",
    "open-picker",
    "from-url",
    "help",
    "--help",
    "-h",
];

fn run(mut args: Vec<String>, json_out: bool) -> Result<()> {
    if json_out {
        if let Some(command) = PLAIN_ONLY
            .iter()
            .find(|c| args.first().map(String::as_str) == Some(**c))
        {
            return Err(api::err(
                "invalid_args",
                format!("{command} has no --json output."),
            ));
        }
        if args.is_empty() {
            return Err(api::err(
                "invalid_args",
                "Name a command; see herdr-portal help.",
            ));
        }
    }
    if args.is_empty() {
        print_help();
        return Ok(());
    }
    if args[0] == "pick" {
        if let Ok(port) = env::var("HERDR_PORTAL_PORT") {
            if !args.iter().any(|a| a == "--port") {
                args.splice(1..1, ["--port".into(), port]);
            }
        }
    }
    match args[0].as_str() {
        "scan" => api::cmd_scan(&args[1..], json_out),
        "pick" => picker::run(&args[1..]),
        "add" => api::cmd_add(&args[1..], json_out),
        "list" => api::cmd_list(&args[1..], json_out),
        "progress" => progress::cmd_progress(&args[1..], json_out),
        "event" => event::cmd_event(&args[1..], json_out),
        "stop" => api::cmd_stop(&args[1..], json_out),
        "start" => api::cmd_start(&args[1..], json_out),
        "request" => requests::cmd_request(&args[1..], json_out),
        "release" => requests::cmd_release(&args[1..], json_out),
        "requests" => requests::cmd_requests(&args[1..], json_out),
        "watch" => requests::cmd_watch(&args[1..], json_out),
        "start-watch" => requests::cmd_start_watch(&args[1..], json_out),
        "restore" => cmd_restore(),
        "open-picker" => cmd_open_picker(&args[1..]),
        "from-url" => cmd_from_url(),
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        other => {
            if !json_out {
                print_help();
            }
            Err(api::err("invalid_args", format!("unknown command {other}")))
        }
    }
}

fn print_help() {
    println!(
        "herdr-portal — open a remote Herdr workspace port on this laptop

  URL: http://herdr.{{workspace}}.localhost:{{port}}

  scan <Local|machine> [--pane ID] [--agent PANE|NAME] [--workspace ID|LABEL]
       [--state active|inactive|available]
  pick [--port N] [Local|machine]
  add <machine> <port> [--local-port N] [--workspace NAME] [--pane ID] [--open]
  list [--state active|inactive]
  stop <local-port> [--keep]
  start <local-port>
  restore
  progress <Local|machine>   herdr-projects progress and Claude Code teams there
  event <kind> --work <id> …  append one work-history event here (see README: Work history)

Agent requests (run in a Herdr pane on the machine where the service listens):
  request <port> [--local-port N | --any] [--pane ID] [--wait SECS]
  release <port> [--pane ID]
  requests [--all | --pane ID]
  watch [--once | --status] [--restore-first] [--interval MS]
                   on the laptop: open and close forwards that panes ask for
  start-watch      start watch in the background unless it already runs

Add --json to scan, list, add, stop, start or progress for one JSON object on stdout:
{{\"ok\": true, ...}} on success, {{\"ok\": false, \"error\": {{\"code\", \"message\"}}}} on failure.

Forwards outlive the picker. Herdr startup runs `watch --restore-first`.
Select Local in the Herdr sidebar before picking a remote machine.
"
    );
}

fn herdr_bin() -> String {
    env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".into())
}

fn local_machine() -> Machine {
    Machine {
        id: LOCAL_ID.into(),
        label: "Local".into(),
        target: String::new(),
        session: String::new(),
    }
}

fn is_local(machine: &Machine) -> bool {
    machine.id == LOCAL_ID || machine.target.is_empty()
}

fn workspace_slug(label: &str) -> String {
    let mut slug = String::new();
    let mut dash = false;
    for ch in label.trim().to_ascii_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            dash = false;
        } else if !dash && !slug.is_empty() {
            slug.push('-');
            dash = true;
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "workspace".into()
    } else {
        slug
    }
}

fn workspace_url(label: &str, port: u16) -> String {
    format!("http://herdr.{}.localhost:{port}", workspace_slug(label))
}

fn state_dir() -> Result<PathBuf> {
    let path = match env::var_os("HERDR_PLUGIN_STATE_DIR") {
        Some(value) => PathBuf::from(value),
        // Same directory herdr passes the plugin, so shell runs and the popup share state.
        None => match env::var_os("XDG_STATE_HOME") {
            Some(base) if !base.is_empty() => PathBuf::from(base),
            _ => home_dir()?.join(".local/state"),
        }
        .join("herdr/plugins")
        .join(PLUGIN_ID),
    };
    fs::create_dir_all(path.join("ssh"))?;
    Ok(path)
}

fn home_dir() -> Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("HOME is unset"))
}

fn forwards_path() -> Result<PathBuf> {
    Ok(state_dir()?.join("forwards.json"))
}

fn load_forwards() -> Result<Vec<Forward>> {
    let path = forwards_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&text).unwrap_or_default())
}

fn save_forwards(items: &[Forward]) -> Result<()> {
    fs::write(
        forwards_path()?,
        serde_json::to_string_pretty(items)? + "\n",
    )?;
    Ok(())
}

fn run_cmd(args: &[&str], input: Option<&str>) -> Result<(i32, String, String)> {
    let mut cmd = Command::new(args[0]);
    cmd.args(&args[1..])
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| spawn_failure(args[0], &e))?;
    if let Some(text) = input {
        if let Some(stdin) = child.stdin.as_mut() {
            stdin.write_all(text.as_bytes())?;
        }
    }
    let output = child.wait_with_output()?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let code = output.status.code().unwrap_or(1);
    Ok((code, stdout, stderr))
}

/// A command that would not start. `--json` names the tool, never the path it was looked up at (`HERDR_BIN_PATH` can
/// sit in a home folder); the plain CLI shows both.
fn spawn_failure(program: &str, error: &std::io::Error) -> anyhow::Error {
    let detail = format!("{program}: {error}");
    if program == herdr_bin() {
        return api::err_with("herdr_failed", "Could not run herdr.", detail);
    }
    match Path::new(program).file_name().and_then(|n| n.to_str()) {
        Some("ssh") => api::err_with("ssh_failed", "Could not run ssh.", detail),
        Some(name) => api::err_with("internal", &format!("Could not run {name}."), detail),
        None => api::err_with("internal", "Could not run a command.", detail),
    }
}

fn load_machines() -> Result<Vec<Machine>> {
    let bin = herdr_bin();
    let (code, stdout, _) = run_cmd(&[&bin, "machine", "list", "--json"], None)?;
    if code != 0 {
        return Ok(Vec::new());
    }
    let rows: Vec<MachineRow> = serde_json::from_str(stdout.trim()).unwrap_or_default();
    Ok(rows
        .into_iter()
        .filter(|row| row.enabled)
        .map(|row| Machine {
            id: row.id,
            label: row.label,
            target: row.target,
            session: if row.session.is_empty() {
                "default".into()
            } else {
                row.session
            },
        })
        .collect())
}

fn load_targets() -> Result<Vec<Machine>> {
    let mut targets = vec![local_machine()];
    targets.extend(load_machines()?);
    Ok(targets)
}

fn resolve_machine(selector: &str) -> Result<Machine> {
    if selector.eq_ignore_ascii_case("local") || selector == LOCAL_ID || selector == "." {
        return Ok(local_machine());
    }
    let targets = load_targets()?;
    let matches: Vec<_> = targets
        .into_iter()
        .filter(|m| m.id == selector || m.label == selector || m.target == selector)
        .collect();
    match matches.len() {
        1 => Ok(matches.into_iter().next().unwrap()),
        0 => Err(api::err_with(
            "machine_not_found",
            "Unknown machine. Use Local or `herdr machine list`.",
            format!("Unknown machine '{selector}'. Use Local or `herdr machine list`."),
        )),
        _ => Err(api::err_with(
            "invalid_args",
            "More than one machine matches; use the profile id.",
            format!("Machine '{selector}' is ambiguous; use the profile id."),
        )),
    }
}

fn parse_listeners(text: &str) -> Vec<Listener> {
    let ss = parse_ss(text);
    if text.lines().any(|line| line.contains("LISTEN")) && !ss.is_empty() {
        return dedupe(ss);
    }
    let lsof = parse_lsof(text);
    if !lsof.is_empty() {
        return dedupe(lsof);
    }
    dedupe(ss)
}

fn parse_ss(text: &str) -> Vec<Listener> {
    let re =
        Regex::new(r"LISTEN\s+\S+\s+\S+\s+(\S+):(\d+)\s+\S+(?:\s+users:\(\((.+)\)\))?").unwrap();
    let pid_re = Regex::new(r"pid=(\d+)").unwrap();
    text.lines()
        .filter_map(|line| {
            let caps = re.captures(line)?;
            let addr = caps.get(1)?.as_str().to_string();
            let port: u16 = caps.get(2)?.as_str().parse().ok()?;
            let users = caps.get(3).map(|m| m.as_str()).unwrap_or("");
            let process = users
                .split(',')
                .next()
                .unwrap_or("unknown")
                .trim()
                .trim_matches('"')
                .to_string();
            let pid = pid_re
                .captures(users)
                .and_then(|c| c.get(1)?.as_str().parse().ok());
            Some(Listener {
                port,
                addr,
                process: if process.is_empty() {
                    "unknown".into()
                } else {
                    process
                },
                pid,
            })
        })
        .collect()
}

fn parse_lsof(text: &str) -> Vec<Listener> {
    let re = Regex::new(
        r"^(\S+)\s+(\d+)\s+\S+\s+\S+\s+\S+\s+\S+\s+\S+\s+TCP\s+(\S+):(\d+)\s+\(LISTEN\)",
    )
    .unwrap();
    text.lines()
        .filter_map(|line| {
            let caps = re.captures(line)?;
            Some(Listener {
                process: caps.get(1)?.as_str().to_string(),
                pid: caps.get(2)?.as_str().parse().ok(),
                addr: caps.get(3)?.as_str().to_string(),
                port: caps.get(4)?.as_str().parse().ok()?,
            })
        })
        .collect()
}

fn dedupe(items: Vec<Listener>) -> Vec<Listener> {
    let skip: HashSet<&str> = ["sshd", "ssh", "herdr-portal"].into_iter().collect();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for item in items {
        if skip.contains(item.process.as_str()) {
            continue;
        }
        let key = (item.addr.clone(), item.port);
        if seen.insert(key) {
            out.push(item);
        }
    }
    out.sort_by_key(|item| (item.port, item.addr.clone()));
    out
}

fn port_free(host: &str, port: u16) -> bool {
    TcpListener::bind((host, port)).is_ok()
}

fn local_port_holder(port: u16) -> Option<String> {
    if port_free("127.0.0.1", port) && port_free("::1", port) {
        return None;
    }
    if let Ok((0, stdout, _)) = run_cmd(
        &["lsof", "-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN"],
        None,
    ) {
        if let Some(line) = stdout.lines().nth(1) {
            let parts: Vec<_> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                return Some(format!("{} pid {}", parts[0], parts[1]));
            }
        }
    }
    Some("another process".into())
}

fn control_path(target: &str) -> Result<PathBuf> {
    // macOS sockaddr_un.sun_path is 104 bytes. Herdr plugin state dirs are too long.
    let digest = hex_digest(target.as_bytes());
    Ok(PathBuf::from("/tmp").join(format!("hpl-{}", &digest[..12])))
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The fixed `--json` message for each SSH or remote-command failure (see `api::err_with`).
pub(crate) const SSH_UNREACHABLE: &str = "Could not reach the machine over SSH.";
pub(crate) const SSH_MASTER_FAILED: &str = "The SSH connection to the machine did not start.";
pub(crate) const SSH_FORWARD_FAILED: &str = "SSH could not open the forward.";
pub(crate) const SSH_STOP_FAILED: &str = "SSH could not stop the forward.";
pub(crate) const HERDR_FAILED: &str = "herdr on the machine failed.";
pub(crate) const LISTENERS_FAILED: &str = "Could not list the listening ports.";
pub(crate) const MASTER_UNREADABLE: &str = "Could not read the SSH master's process.";

/// A failed command run over SSH: exit 255 is ssh's own (it could not connect or log in), any other code the remote
/// command's. The plain CLI keeps `detail`; JSON gets only the fixed message.
pub(crate) fn remote_failure(
    code: i32,
    otherwise: &'static str,
    message: &str,
    detail: impl Into<String>,
) -> anyhow::Error {
    if code == 255 {
        api::err_with("ssh_failed", SSH_UNREACHABLE, detail)
    } else {
        api::err_with(otherwise, message, detail)
    }
}

/// ssh's or a command's stderr, else its stdout: what a person at the plain CLI sees.
fn said(stdout: &str, stderr: &str) -> String {
    if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    }
    .to_string()
}

fn ssh_base(target: &str) -> Result<Vec<String>> {
    Ok(vec![
        "ssh".into(),
        "-T".into(),
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "ConnectTimeout=10".into(),
        "-o".into(),
        "StrictHostKeyChecking=yes".into(),
        "-o".into(),
        format!("ControlPath={}", control_path(target)?.display()),
    ])
}

fn ensure_master(target: &str) -> Result<()> {
    if master_alive(target)? {
        return Ok(());
    }
    spawn_master(target)?;
    for _ in 0..50 {
        if master_alive(target)? {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Err(api::err_with(
        "ssh_failed",
        SSH_MASTER_FAILED,
        format!("ssh master did not start for {target}"),
    ))
}

fn master_alive(target: &str) -> Result<bool> {
    let mut check = ssh_base(target)?;
    check.extend(["-O".into(), "check".into(), target.into()]);
    let args: Vec<&str> = check.iter().map(String::as_str).collect();
    Ok(run_cmd(&args, None)?.0 == 0)
}

fn spawn_master(target: &str) -> Result<()> {
    let control = control_path(target)?;
    let mut cmd = Command::new("ssh");
    cmd.args([
        "-N",
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=10",
        "-o",
        "StrictHostKeyChecking=yes",
        "-o",
        &format!("ControlPath={}", control.display()),
        "-o",
        "ControlMaster=yes",
        "-o",
        "ControlPersist=yes",
        "-o",
        "ServerAliveInterval=30",
        "-o",
        "ServerAliveCountMax=3",
        target,
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::signal(libc::SIGHUP, libc::SIG_IGN);
                Ok(())
            });
        }
    }
    let child = cmd.spawn().context("ssh")?;
    std::mem::forget(child);
    Ok(())
}

fn run_on(machine: &Machine, argv: &[&str], input: Option<&str>) -> Result<(i32, String, String)> {
    if is_local(machine) {
        return run_cmd(argv, input);
    }
    ensure_master(&machine.target)?;
    let mut args = ssh_base(&machine.target)?;
    args.push(machine.target.clone());
    args.extend(argv.iter().map(|s| (*s).to_string()));
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run_cmd(&refs, input)
}

fn herdr_argv(machine: &Machine, args: &[&str]) -> Vec<String> {
    let mut cmd = vec![if is_local(machine) {
        herdr_bin()
    } else {
        env::var("HERDR_PORTAL_REMOTE_HERDR").unwrap_or_else(|_| "herdr".into())
    }];
    if !is_local(machine) && !machine.session.is_empty() {
        cmd.extend(["--session".into(), machine.session.clone()]);
    }
    cmd.extend(args.iter().map(|s| (*s).to_string()));
    cmd
}

fn parse_json_blob(text: &str) -> Result<Value> {
    let start = text.find('{').ok_or_else(|| anyhow!("no JSON object"))?;
    Ok(serde_json::from_str(&text[start..])?)
}

fn herdr_result(machine: &Machine, args: &[&str]) -> Result<Value> {
    let argv = herdr_argv(machine, args);
    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let (code, stdout, stderr) = run_on(machine, &refs, None)?;
    if code != 0 {
        let detail = said(&stdout, &stderr);
        return Err(if is_local(machine) {
            api::err_with("herdr_failed", HERDR_FAILED, detail)
        } else {
            remote_failure(code, "herdr_failed", HERDR_FAILED, detail)
        });
    }
    let payload = parse_json_blob(&stdout)?;
    Ok(payload.get("result").cloned().unwrap_or(payload))
}

fn scan_listeners(machine: &Machine) -> Result<Vec<Listener>> {
    if is_local(machine) {
        let (code, stdout, stderr) = if Path::new("/usr/sbin/lsof").exists() || which("lsof") {
            run_cmd(&["lsof", "-nP", "-iTCP", "-sTCP:LISTEN"], None)?
        } else {
            run_cmd(&["ss", "-ltnpH"], None)?
        };
        if code != 0 {
            return Err(api::err_with("internal", LISTENERS_FAILED, stderr.trim()));
        }
        return Ok(parse_listeners(&stdout));
    }
    ensure_master(&machine.target)?;
    let script = r#"
set -e
if command -v ss >/dev/null 2>&1; then
  ss -ltnpH 2>/dev/null || ss -ltnp
elif command -v lsof >/dev/null 2>&1; then
  lsof -nP -iTCP -sTCP:LISTEN
else
  echo 'remote host needs ss or lsof' >&2
  exit 1
fi
"#;
    let mut args = ssh_base(&machine.target)?;
    args.extend([machine.target.clone(), "bash".into(), "-s".into()]);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let (code, stdout, stderr) = run_cmd(&refs, Some(script))?;
    if code != 0 {
        return Err(remote_failure(
            code,
            "internal",
            LISTENERS_FAILED,
            stderr.trim(),
        ));
    }
    Ok(parse_listeners(&stdout))
}

fn which(name: &str) -> bool {
    env::var_os("PATH")
        .map(|paths| env::split_paths(&paths).any(|dir| dir.join(name).exists()))
        .unwrap_or(false)
}

fn pid_parent_map(machine: &Machine) -> Result<HashMap<u32, u32>> {
    let (code, stdout, _) = run_on(machine, &["ps", "-axo", "pid=,ppid="], None)?;
    let mut map = HashMap::new();
    if code != 0 {
        return Ok(map);
    }
    for line in stdout.lines() {
        let parts: Vec<_> = line.split_whitespace().collect();
        if parts.len() == 2 {
            if let (Ok(pid), Ok(ppid)) = (parts[0].parse(), parts[1].parse()) {
                map.insert(pid, ppid);
            }
        }
    }
    Ok(map)
}

fn ancestors(pid: u32, parents: &HashMap<u32, u32>) -> HashSet<u32> {
    let mut seen = HashSet::new();
    let mut current = Some(pid);
    while let Some(pid) = current {
        if !seen.insert(pid) {
            break;
        }
        current = parents.get(&pid).copied();
    }
    seen
}

#[derive(Clone, Debug, Default)]
struct PaneRef {
    tab_id: Option<String>,
    pane_id: String,
    agent: Option<String>,
    agent_name: Option<String>,
}

fn attribute_ports(machine: &Machine, listeners: &[Listener]) -> Result<Vec<WorkspacePort>> {
    let parents = pid_parent_map(machine)?;
    let workspaces = herdr_result(machine, &["workspace", "list"])?;
    let mut labels = HashMap::new();
    if let Some(rows) = workspaces.get("workspaces").and_then(Value::as_array) {
        for row in rows {
            if let (Some(id), label) = (
                row.get("workspace_id").and_then(Value::as_str),
                row.get("label").and_then(Value::as_str),
            ) {
                labels.insert(id.to_string(), label.unwrap_or(id).to_string());
            }
        }
    }
    let mut agent_names = HashMap::new();
    if let Ok(agents) = herdr_result(machine, &["agent", "list"]) {
        for row in agents
            .get("agents")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let (Some(pane), Some(name)) = (
                row.get("pane_id").and_then(Value::as_str),
                row.get("name").and_then(Value::as_str),
            ) {
                agent_names.insert(pane.to_string(), name.to_string());
            }
        }
    }
    let panes = herdr_result(machine, &["pane", "list"])?;
    let mut pane_index: Vec<(HashSet<u32>, String, PaneRef)> = Vec::new();
    let mut workspace_paths = Vec::new();
    if let Some(rows) = panes.get("panes").and_then(Value::as_array) {
        for pane in rows {
            let Some(pane_id) = pane.get("pane_id").and_then(Value::as_str) else {
                continue;
            };
            let workspace_id = pane
                .get("workspace_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            for key in ["cwd", "foreground_cwd"] {
                if let Some(path) = pane.get(key).and_then(Value::as_str) {
                    if Path::new(path).is_absolute() && Path::new(path).parent().is_some() {
                        workspace_paths.push((PathBuf::from(path), workspace_id.clone()));
                    }
                }
            }
            let info = herdr_result(machine, &["pane", "process-info", "--pane", pane_id])?;
            let process_info = info.get("process_info").cloned().unwrap_or(info);
            let mut pids = HashSet::new();
            if let Some(pid) = process_info.get("shell_pid").and_then(Value::as_u64) {
                pids.insert(pid as u32);
            }
            if let Some(procs) = process_info
                .get("foreground_processes")
                .and_then(Value::as_array)
            {
                for proc in procs {
                    if let Some(pid) = proc.get("pid").and_then(Value::as_u64) {
                        pids.insert(pid as u32);
                    }
                }
            }
            let pane_ref = PaneRef {
                tab_id: pane
                    .get("tab_id")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                pane_id: pane_id.to_string(),
                agent: pane
                    .get("agent")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                agent_name: agent_names.get(pane_id).cloned(),
            };
            pane_index.push((pids, workspace_id, pane_ref));
        }
    }
    let mut found = Vec::new();
    let mut seen = HashSet::new();
    let paths = listener_paths(machine, listeners)?;
    for listener in listeners {
        let Some(pid) = listener.pid else { continue };
        let tree = ancestors(pid, &parents);
        let matched = pane_index
            .iter()
            .find(|(pids, _, _)| !pids.is_disjoint(&tree));
        let pane_ref = matched.map(|(_, _, pane)| pane.clone());
        let workspace_id = if let Some((_, id, _)) = matched {
            Some(id.clone())
        } else {
            paths
                .get(&pid)
                .and_then(|paths| workspace_for_paths(paths, &workspace_paths))
        };
        let Some(workspace_id) = workspace_id else {
            continue;
        };
        let label = labels.get(&workspace_id).unwrap_or(&workspace_id);
        if !seen.insert((workspace_id.clone(), listener.port)) {
            continue;
        }
        let pane_ref = pane_ref.unwrap_or_default();
        found.push(WorkspacePort {
            workspace_id: workspace_id.clone(),
            workspace_label: label.clone(),
            port: listener.port,
            process: listener.process.clone(),
            tab_id: pane_ref.tab_id,
            pane_id: (!pane_ref.pane_id.is_empty()).then_some(pane_ref.pane_id),
            agent: pane_ref.agent,
            agent_name: pane_ref.agent_name,
            pid: Some(pid),
        });
    }
    found.sort_by(|a, b| {
        a.workspace_label
            .cmp(&b.workspace_label)
            .then(a.port.cmp(&b.port))
    });
    Ok(found)
}

// Detached servers can outlive the pane that launched them. Use path evidence
// only when ancestry cannot identify a workspace; ambiguous matches stay hidden.
fn listener_paths(machine: &Machine, listeners: &[Listener]) -> Result<HashMap<u32, Vec<PathBuf>>> {
    let pids = listeners
        .iter()
        .filter_map(|l| l.pid)
        .collect::<HashSet<_>>()
        .into_iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>();
    if pids.is_empty() {
        return Ok(HashMap::new());
    }
    let list = pids.join(",");
    let space_list = pids.join(" ");
    let script = format!(
        "if command -v lsof >/dev/null 2>&1; then lsof -nP -a -p {list} -d cwd -Fn 2>/dev/null; else for pid in {space_list}; do printf 'p%s\\n' \"$pid\"; readlink /proc/$pid/cwd 2>/dev/null | sed 's/^/n/'; done; fi\nps -p {list} -o pid=,command=\n"
    );
    let (_, stdout, _) = run_on(machine, &["sh", "-s"], Some(&script))?;
    let mut paths: HashMap<u32, Vec<PathBuf>> = HashMap::new();
    let mut pid = None;
    for line in stdout.lines() {
        if let Some(value) = line.strip_prefix('p').and_then(|p| p.parse::<u32>().ok()) {
            pid = Some(value);
        } else if let Some(path) = line.strip_prefix("n/") {
            if let Some(pid) = pid {
                paths
                    .entry(pid)
                    .or_default()
                    .push(PathBuf::from(format!("/{path}")));
            }
        } else if let Some((pid, command)) = line.trim_start().split_once(char::is_whitespace) {
            if let (Ok(pid), Some(path)) = (pid.parse::<u32>(), python_server_directory(command)) {
                paths.entry(pid).or_default().push(path);
            }
        }
    }
    Ok(paths)
}

fn python_server_directory(command: &str) -> Option<PathBuf> {
    let args = command.split_whitespace().collect::<Vec<_>>();
    if !args.windows(2).any(|pair| pair == ["-m", "http.server"]) {
        return None;
    }
    let path = args
        .windows(2)
        .find(|pair| pair[0] == "--directory" || pair[0] == "-d")?[1];
    Path::new(path).is_absolute().then(|| PathBuf::from(path))
}

fn workspace_for_paths(paths: &[PathBuf], workspaces: &[(PathBuf, String)]) -> Option<String> {
    let matches = workspaces
        .iter()
        .filter(|(root, _)| paths.iter().any(|path| path.starts_with(root)))
        .collect::<Vec<_>>();
    let depth = matches
        .iter()
        .map(|(root, _)| root.components().count())
        .max()?;
    let ids = matches
        .iter()
        .filter(|(root, _)| root.components().count() == depth)
        .map(|(_, id)| id.as_str())
        .collect::<HashSet<_>>();
    (ids.len() == 1).then(|| ids.into_iter().next().unwrap_or_default().to_owned())
}

fn spec(local_port: u16, remote_port: u16) -> String {
    format!("127.0.0.1:{local_port}:127.0.0.1:{remote_port}")
}

fn existing_forward(machine: &Machine, remote_port: u16) -> Result<Option<Forward>> {
    Ok(load_forwards()?.into_iter().find(|item| {
        (item.machine_id == machine.id
            || (!machine.target.is_empty() && item.target == machine.target))
            && item.remote_port == remote_port
    }))
}

fn forward_alive(forward: &Forward) -> Result<bool> {
    if forward.target.is_empty() {
        return Ok(false);
    }
    let mut args = ssh_base(&forward.target)?;
    args.extend(["-O".into(), "check".into(), forward.target.clone()]);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let (code, _, stderr) = run_cmd(&refs, None)?;
    if code != 0 {
        return Ok(false);
    }
    let pid = stderr
        .split_once("Master running (pid=")
        .and_then(|(_, rest)| rest.split_once(')'))
        .and_then(|(pid, _)| pid.parse::<u32>().ok())
        .ok_or_else(|| {
            api::err_with(
                "internal",
                MASTER_UNREADABLE,
                format!("Cannot read SSH master PID: {}", stderr.trim()),
            )
        })?;
    // A busy port alone is insufficient: it must belong to this SSH master.
    let listeners = if which("lsof") {
        let (code, stdout, stderr) = run_cmd(
            &[
                "lsof",
                "-nP",
                "-a",
                "-p",
                &pid.to_string(),
                &format!("-iTCP:{}", forward.local_port),
                "-sTCP:LISTEN",
            ],
            None,
        )?;
        if code != 0 && !(code == 1 && stdout.is_empty() && stderr.is_empty()) {
            return Err(api::err_with(
                "internal",
                LISTENERS_FAILED,
                format!("Cannot inspect SSH listeners: {}", stderr.trim()),
            ));
        }
        parse_lsof(&stdout)
    } else {
        let (code, stdout, stderr) = run_cmd(&["ss", "-ltnpH"], None)?;
        if code != 0 {
            return Err(api::err_with(
                "internal",
                LISTENERS_FAILED,
                format!("Cannot inspect SSH listeners: {}", stderr.trim()),
            ));
        }
        parse_ss(&stdout)
    };
    Ok(owns_forward_listener(&listeners, pid, forward.local_port))
}

fn owns_forward_listener(listeners: &[Listener], pid: u32, port: u16) -> bool {
    listeners.iter().any(|listener| {
        listener.pid == Some(pid) && listener.port == port && listener.addr == "127.0.0.1"
    })
}

fn forward_symbol(forward: Option<&Forward>) -> Result<&'static str> {
    match forward {
        None => Ok("○"),
        Some(forward) if forward_alive(forward)? => Ok("●"),
        Some(_) => Ok("◐"),
    }
}

fn merge_forwarded_ports(
    machine: &Machine,
    mut ports: Vec<WorkspacePort>,
) -> Result<Vec<WorkspacePort>> {
    let mut have: HashSet<(String, u16)> = ports
        .iter()
        .map(|p| (p.workspace_label.clone(), p.port))
        .collect();
    for fwd in load_forwards()? {
        let same = fwd.machine_id == machine.id
            || (!machine.target.is_empty() && fwd.target == machine.target);
        if !same {
            continue;
        }
        let label = if fwd.workspace_label.is_empty() {
            fwd.label.clone()
        } else {
            fwd.workspace_label.clone()
        };
        if have.insert((label.clone(), fwd.remote_port)) {
            ports.push(WorkspacePort {
                workspace_id: String::new(),
                workspace_label: label,
                port: fwd.remote_port,
                process: "forwarded".into(),
                pane_id: fwd.pane_id.clone(),
                ..Default::default()
            });
        }
    }
    Ok(ports)
}

fn add_forward(
    machine: &Machine,
    remote_port: u16,
    local_port: Option<u16>,
    workspace_label: &str,
    pane_id: Option<&str>,
) -> Result<Forward> {
    let local_port = local_port.unwrap_or(remote_port);
    if let Some(holder) = local_port_holder(local_port) {
        return Err(api::err(
            "local_port_in_use",
            format!("Local port {local_port} is held by {holder}. Pass --local-port PORT."),
        ));
    }
    let pane_id = pane_id.map(str::to_string);
    if is_local(machine) {
        let forward = Forward {
            machine_id: machine.id.clone(),
            label: machine.label.clone(),
            target: String::new(),
            remote_port,
            local_port,
            workspace_label: workspace_label.to_string(),
            pane_id,
            origin: Origin::Manual,
        };
        let mut items = load_forwards()?;
        items.retain(|item| !(item.machine_id == LOCAL_ID && item.local_port == local_port));
        items.push(forward.clone());
        save_forwards(&items)?;
        return Ok(forward);
    }
    ensure_master(&machine.target)?;
    let mut args = ssh_base(&machine.target)?;
    args.extend([
        "-O".into(),
        "forward".into(),
        "-L".into(),
        spec(local_port, remote_port),
        machine.target.clone(),
    ]);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let (code, _, stderr) = run_cmd(&refs, None)?;
    if code != 0 {
        return Err(api::err_with(
            "ssh_failed",
            SSH_FORWARD_FAILED,
            stderr.trim(),
        ));
    }
    let mut items = load_forwards()?;
    items.retain(|item| !(item.target == machine.target && item.local_port == local_port));
    let forward = Forward {
        machine_id: machine.id.clone(),
        label: machine.label.clone(),
        target: machine.target.clone(),
        remote_port,
        local_port,
        workspace_label: workspace_label.to_string(),
        pane_id,
        origin: Origin::Manual,
    };
    items.push(forward.clone());
    save_forwards(&items)?;
    Ok(forward)
}

// With `keep`, the forward stays saved (inactive) so `start` or `restore` can resume it.
fn stop_forward(local_port: u16, keep: bool) -> Result<Forward> {
    let items = load_forwards()?;
    let Some(matched) = items
        .iter()
        .find(|item| item.local_port == local_port)
        .cloned()
    else {
        return Err(api::err(
            "no_forward",
            format!("No tracked forward on local port {local_port}"),
        ));
    };
    if !matched.target.is_empty() {
        let mut args = ssh_base(&matched.target)?;
        args.extend([
            "-O".into(),
            "cancel".into(),
            "-L".into(),
            spec(matched.local_port, matched.remote_port),
            matched.target.clone(),
        ]);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let (code, _, stderr) = run_cmd(&refs, None)?;
        if code != 0 && forward_alive(&matched)? {
            return Err(api::err_with(
                "ssh_failed",
                SSH_STOP_FAILED,
                format!("Cannot stop forward: {}", stderr.trim()),
            ));
        }
    }
    if !keep {
        let rest: Vec<_> = items
            .into_iter()
            .filter(|item| item.local_port != local_port)
            .collect();
        save_forwards(&rest)?;
    }
    Ok(matched)
}

fn open_browser(url: &str) -> Result<()> {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = Command::new(opener)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    Ok(())
}

fn format_ports(machine: &Machine, ports: &[WorkspacePort]) -> Result<String> {
    let mut lines = vec![format!("Machine: {}", machine.label)];
    if ports.is_empty() {
        lines.push("No listeners in this workspace, and no tracked forwards.".into());
        return Ok(lines.join("\n"));
    }
    lines.push(format!(
        "{:<4}{:<4}{:<18}{:<8}{:<16}URL",
        "#", "ST", "WORKSPACE", "PORT", "PROCESS"
    ));
    for (index, item) in ports.iter().enumerate() {
        let forward = existing_forward(machine, item.port)?;
        let st = forward_symbol(forward.as_ref())?;
        let local_port = forward.as_ref().map_or(item.port, |f| f.local_port);
        let url = workspace_url(&item.workspace_label, local_port).replacen("http://", "", 1);
        lines.push(format!(
            "{:<4}{st:<4}{:<18}{:<8}{:<16}{url}",
            index + 1,
            item.workspace_label,
            item.port,
            item.process
        ));
    }
    lines.push("○ closed  ◐ closed but saved  ● saved and alive".into());
    Ok(lines.join("\n"))
}

fn load_workspaces(machine: &Machine) -> Result<Vec<(String, String)>> {
    let result = herdr_result(machine, &["workspace", "list"])?;
    let mut out = Vec::new();
    if let Some(rows) = result.get("workspaces").and_then(Value::as_array) {
        for row in rows {
            if let Some(id) = row.get("workspace_id").and_then(Value::as_str) {
                let label = row
                    .get("label")
                    .and_then(Value::as_str)
                    .unwrap_or(id)
                    .to_string();
                out.push((id.to_string(), label));
            }
        }
    }
    Ok(out)
}

fn cmd_restore() -> Result<()> {
    let items = load_forwards()?;
    if items.is_empty() {
        println!("No tracked forwards to restore.");
        return Ok(());
    }
    let mut ok = 0usize;
    for item in &items {
        if item.target.is_empty() {
            continue;
        }
        match restore_one(item) {
            Ok(()) => {
                ok += 1;
                let label = if item.workspace_label.is_empty() {
                    &item.label
                } else {
                    &item.workspace_label
                };
                println!(
                    "{} {} -> {}",
                    forward_symbol(Some(item))?,
                    item.remote_port,
                    workspace_url(label, item.local_port)
                );
            }
            Err(err) => println!("skip {}:{}: {err:#}", item.label, item.remote_port),
        }
    }
    println!("Restored {ok} forward(s).");
    Ok(())
}

fn restore_one(item: &Forward) -> Result<()> {
    if forward_alive(item)? {
        return Ok(());
    }
    ensure_master(&item.target)?;
    if let Some(holder) = local_port_holder(item.local_port) {
        return Err(api::err(
            "local_port_in_use",
            format!("Local port {} is held by {holder}.", item.local_port),
        ));
    }
    let mut args = ssh_base(&item.target)?;
    args.extend([
        "-O".into(),
        "forward".into(),
        "-L".into(),
        spec(item.local_port, item.remote_port),
        item.target.clone(),
    ]);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let (code, _, stderr) = run_cmd(&refs, None)?;
    if code != 0 {
        return Err(api::err_with(
            "ssh_failed",
            SSH_FORWARD_FAILED,
            stderr.trim(),
        ));
    }
    Ok(())
}

fn cmd_open_picker(args: &[String]) -> Result<()> {
    let bin = herdr_bin();
    let mut argv = vec![
        bin.as_str(),
        "plugin",
        "pane",
        "open",
        "--plugin",
        PLUGIN_ID,
        "--entrypoint",
        "picker",
        "--placement",
        "popup",
    ];
    let env_arg;
    if args.first().map(String::as_str) == Some("--port") && args.len() > 1 {
        env_arg = format!("HERDR_PORTAL_PORT={}", args[1]);
        argv.extend(["--env", env_arg.as_str()]);
    }
    let (code, stdout, stderr) = run_cmd(&argv, None)?;
    if code != 0 {
        eprint!("{}", if stderr.is_empty() { stdout } else { stderr });
        bail!("plugin pane open failed");
    }
    print!("{stdout}");
    Ok(())
}

fn cmd_from_url() -> Result<()> {
    let url = env::var("HERDR_PLUGIN_CLICKED_URL").ok().or_else(|| {
        env::var("HERDR_PLUGIN_CONTEXT_JSON")
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .and_then(|v| v.get("clicked_url")?.as_str().map(str::to_string))
    });
    let Some(url) = url else {
        bail!("No clicked URL in plugin context.");
    };
    let port = parse_localhost_port(&url).ok_or_else(|| anyhow!("Not a localhost URL: {url}"))?;
    cmd_open_picker(&["--port".into(), port.to_string()])
}

fn parse_localhost_port(url: &str) -> Option<u16> {
    let rest = url
        .strip_prefix("https://")
        .map(|s| ("https", s))
        .or_else(|| url.strip_prefix("http://").map(|s| ("http", s)))?;
    let (scheme, rest) = rest;
    let hostport = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let hostport = hostport.trim_start_matches('[').trim_end_matches(']');
    let (host, port) = if let Some((h, p)) = hostport.rsplit_once(':') {
        if p.chars().all(|c| c.is_ascii_digit()) {
            (h.trim_end_matches(']'), p.parse().ok())
        } else {
            (hostport, None)
        }
    } else {
        (hostport, None)
    };
    let host = host.to_ascii_lowercase();
    let ok =
        host == "localhost" || host == "127.0.0.1" || host == "::1" || host.ends_with(".localhost");
    if !ok {
        return None;
    }
    port.or(match scheme {
        "https" => Some(443),
        "http" => Some(80),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_and_url() {
        assert_eq!(workspace_slug("spark: ~"), "spark");
        assert_eq!(workspace_url("mo", 8000), "http://herdr.mo.localhost:8000");
    }

    #[test]
    fn detached_http_server_matches_served_directory() {
        let path = python_server_directory("/usr/bin/Python -m http.server 8765 --bind 127.0.0.1 --directory /Users/randomradio/mo/docs/topology").unwrap();
        let roots = vec![(PathBuf::from("/Users/randomradio/mo"), "w1".into())];
        assert_eq!(workspace_for_paths(&[path], &roots).as_deref(), Some("w1"));
        assert_eq!(
            workspace_for_paths(&[PathBuf::from("/Users/randomradio/src/mo")], &roots),
            None
        );
        assert_eq!(
            workspace_for_paths(&[PathBuf::from("/Users/randomradio/more")], &roots),
            None
        );
        assert!(python_server_directory("node app.js --directory /Users/randomradio/mo").is_none());
    }

    #[test]
    fn path_attribution_uses_specific_root_and_rejects_ambiguity() {
        let paths = vec![PathBuf::from("/project/app/server")];
        let mut roots = vec![
            (PathBuf::from("/project"), "w1".into()),
            (PathBuf::from("/project/app"), "w2".into()),
        ];
        assert_eq!(workspace_for_paths(&paths, &roots).as_deref(), Some("w2"));
        roots.push((PathBuf::from("/project/app"), "w3".into()));
        assert_eq!(workspace_for_paths(&paths, &roots), None);
    }

    #[test]
    fn parse_ss_skips_sshd() {
        let text = r#"
LISTEN 0 4096 127.0.0.1:4242 0.0.0.0:* users:(("node",pid=2211,fd=23))
LISTEN 0 128 0.0.0.0:22 0.0.0.0:* users:(("sshd",pid=1,fd=3))
"#;
        let listeners = parse_listeners(text);
        assert_eq!(listeners.len(), 1);
        assert_eq!(listeners[0].port, 4242);
        assert_eq!(listeners[0].pid, Some(2211));
    }

    #[test]
    fn forward_listener_requires_matching_owner_address_and_port() {
        let text = "ssh 2211 you 23u IPv4 0xabc 0t0 TCP 127.0.0.1:4242 (LISTEN)\n";
        let listeners = parse_lsof(text);
        assert!(owns_forward_listener(&listeners, 2211, 4242));
        assert!(!owns_forward_listener(&listeners, 2212, 4242));
        assert!(!owns_forward_listener(&listeners, 2211, 4243));
        let ipv6 = parse_ss("LISTEN 0 128 [::1]:4242 [::]:* users:((\"ssh\",pid=2211,fd=23))");
        assert!(!owns_forward_listener(&ipv6, 2211, 4242));
        let ipv4 =
            parse_ss("LISTEN 0 128 127.0.0.1:4242 0.0.0.0:* users:((\"ssh\",pid=2211,fd=23))");
        assert!(owns_forward_listener(&ipv4, 2211, 4242));
    }
}
