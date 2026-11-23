//! Command handlers for scan, list, add, stop and start. With `--json`, each
//! prints one `{"ok": ...}` object; see "API" in README.md.

use super::*;

#[derive(Debug)]
pub(crate) struct ApiError {
    pub code: &'static str,
    pub message: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

pub(crate) fn err(code: &'static str, message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(ApiError {
        code,
        message: message.into(),
    })
}

pub(crate) fn error_json(error: &anyhow::Error) -> Value {
    let code = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<ApiError>())
        .map_or("internal", |api| api.code);
    json!({"ok": false, "error": {"code": code, "message": format!("{error:#}")}})
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    Active,
    Inactive,
    Available,
}

impl State {
    fn as_str(self) -> &'static str {
        match self {
            State::Active => "active",
            State::Inactive => "inactive",
            State::Available => "available",
        }
    }

    fn parse(text: &str) -> Result<Self> {
        match text {
            "active" => Ok(State::Active),
            "inactive" => Ok(State::Inactive),
            "available" => Ok(State::Available),
            other => Err(err(
                "invalid_args",
                format!("unknown state '{other}'; use active, inactive or available"),
            )),
        }
    }

    fn symbol(self) -> &'static str {
        match self {
            State::Active => "●",
            State::Inactive => "◐",
            State::Available => "○",
        }
    }
}

fn forward_state(forward: &Forward) -> Result<State> {
    Ok(if forward_alive(forward)? {
        State::Active
    } else {
        State::Inactive
    })
}

fn forward_workspace(forward: &Forward) -> &str {
    if forward.workspace_label.is_empty() {
        &forward.label
    } else {
        &forward.workspace_label
    }
}

/// Arguments left after the flags a command knows are pulled out.
#[derive(Debug)]
struct Args {
    positional: Vec<String>,
    flags: HashMap<&'static str, String>,
    switches: HashSet<&'static str>,
}

fn parse_args(args: &[String], valued: &[&'static str], switches: &[&'static str]) -> Result<Args> {
    let mut out = Args {
        positional: Vec::new(),
        flags: HashMap::new(),
        switches: HashSet::new(),
    };
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if let Some(name) = valued.iter().find(|name| **name == arg) {
            let value = args
                .get(i + 1)
                .ok_or_else(|| err("invalid_args", format!("{arg} needs a value")))?;
            out.flags.insert(name, value.clone());
            i += 2;
        } else if let Some(name) = switches.iter().find(|name| **name == arg) {
            out.switches.insert(name);
            i += 1;
        } else if arg.starts_with("--") {
            return Err(err("invalid_args", format!("unknown argument {arg}")));
        } else {
            out.positional.push(arg.to_string());
            i += 1;
        }
    }
    Ok(out)
}

fn parse_port(text: &str) -> Result<u16> {
    text.parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(|| err("invalid_args", format!("'{text}' is not a TCP port")))
}

fn forward_json(forward: &Forward, state: State) -> Value {
    json!({
        "machine": forward.label,
        "machine_id": forward.machine_id,
        "remote_port": forward.remote_port,
        "local_port": forward.local_port,
        "state": state.as_str(),
        "url": workspace_url(forward_workspace(forward), forward.local_port),
        "workspace": forward_workspace(forward),
        "workspace_id": Value::Null,
        "tab_id": Value::Null,
        "pane_id": forward.pane_id,
        "agent": Value::Null,
        "agent_name": Value::Null,
        "process": Value::Null,
        "pid": Value::Null,
    })
}

fn scan_json(
    machine: &Machine,
    item: &WorkspacePort,
    forward: Option<&Forward>,
    state: State,
) -> Value {
    let local_port = forward.map(|f| f.local_port);
    json!({
        "machine": machine.label,
        "machine_id": machine.id,
        "remote_port": item.port,
        "local_port": local_port,
        "state": state.as_str(),
        "url": workspace_url(&item.workspace_label, local_port.unwrap_or(item.port)),
        "workspace": item.workspace_label,
        "workspace_id": (!item.workspace_id.is_empty()).then_some(&item.workspace_id),
        "tab_id": item.tab_id,
        "pane_id": item.pane_id.clone().or_else(|| forward.and_then(|f| f.pane_id.clone())),
        "agent": item.agent,
        "agent_name": item.agent_name,
        "process": item.process,
        "pid": item.pid,
        // Fields from the first scan --json format, kept for existing callers.
        "port": item.port,
        "saved": forward.is_some(),
        "forwarded": state == State::Active,
        "status": state.symbol(),
    })
}

fn print_json(value: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn matches_agent(item: &WorkspacePort, selector: &str) -> bool {
    item.pane_id.as_deref() == Some(selector) || item.agent_name.as_deref() == Some(selector)
}

pub(crate) fn cmd_scan(args: &[String], json_out: bool) -> Result<()> {
    let args = parse_args(args, &["--pane", "--agent", "--workspace", "--state"], &[])?;
    let [selector] = args.positional.as_slice() else {
        return Err(err(
            "invalid_args",
            "usage: herdr-portal scan <Local|machine> [--pane ID] [--agent PANE|NAME] [--workspace ID|LABEL] [--state STATE] [--json]",
        ));
    };
    let want_state = args
        .flags
        .get("--state")
        .map(|s| State::parse(s))
        .transpose()?;
    let machine = resolve_machine(selector)?;
    let ports = merge_forwarded_ports(
        &machine,
        attribute_ports(&machine, &scan_listeners(&machine)?)?,
    )?;
    let filtered = ports.iter().filter(|item| {
        args.flags
            .get("--pane")
            .is_none_or(|pane| item.pane_id.as_deref() == Some(pane))
            && args
                .flags
                .get("--agent")
                .is_none_or(|agent| matches_agent(item, agent))
            && args
                .flags
                .get("--workspace")
                .is_none_or(|ws| item.workspace_id == *ws || item.workspace_label == *ws)
    });
    let mut rows = Vec::new();
    let mut kept = Vec::new();
    for item in filtered {
        let forward = existing_forward(&machine, item.port)?;
        let state = match &forward {
            None => State::Available,
            Some(forward) => forward_state(forward)?,
        };
        if want_state.is_some_and(|want| want != state) {
            continue;
        }
        rows.push(scan_json(&machine, item, forward.as_ref(), state));
        kept.push(item.clone());
    }
    if json_out {
        return print_json(&json!({"ok": true, "machine": machine.label, "ports": rows}));
    }
    println!("{}", format_ports(&machine, &kept)?);
    Ok(())
}

pub(crate) fn cmd_list(args: &[String], json_out: bool) -> Result<()> {
    let args = parse_args(args, &["--state"], &[])?;
    if !args.positional.is_empty() {
        return Err(err(
            "invalid_args",
            "usage: herdr-portal list [--state active|inactive] [--json]",
        ));
    }
    let want_state = args
        .flags
        .get("--state")
        .map(|s| State::parse(s))
        .transpose()?;
    if want_state == Some(State::Available) {
        return Err(err(
            "invalid_args",
            "list only holds saved forwards; use `scan <machine> --state available`",
        ));
    }
    let mut rows = Vec::new();
    for forward in load_forwards()? {
        let state = forward_state(&forward)?;
        if want_state.is_none_or(|want| want == state) {
            rows.push((forward, state));
        }
    }
    if json_out {
        let ports: Vec<Value> = rows.iter().map(|(f, s)| forward_json(f, *s)).collect();
        return print_json(&json!({"ok": true, "ports": ports}));
    }
    if rows.is_empty() {
        println!("No tracked forwards.");
        return Ok(());
    }
    println!("{:<4}{:<8}{:<8}URL", "ST", "LOCAL", "REMOTE");
    for (forward, state) in rows {
        println!(
            "{:<4}{:<8}{:<8}{}",
            state.symbol(),
            forward.local_port,
            forward.remote_port,
            workspace_url(forward_workspace(&forward), forward.local_port)
        );
    }
    Ok(())
}

pub(crate) fn cmd_add(args: &[String], json_out: bool) -> Result<()> {
    let args = parse_args(
        args,
        &["--local-port", "--workspace", "--pane"],
        &["--open"],
    )?;
    let [selector, port] = args.positional.as_slice() else {
        return Err(err(
            "invalid_args",
            "usage: herdr-portal add <machine> <port> [--local-port N] [--workspace NAME] [--pane ID] [--open] [--json]",
        ));
    };
    let remote_port = parse_port(port)?;
    let local_port = args
        .flags
        .get("--local-port")
        .map(|p| parse_port(p))
        .transpose()?;
    let machine = resolve_machine(selector)?;

    // Asking again for a forward that is already up returns it unchanged.
    if let Some(existing) = existing_forward(&machine, remote_port)? {
        if local_port.is_none_or(|p| p == existing.local_port) && forward_alive(&existing)? {
            return report_add(
                &machine,
                &existing,
                json_out,
                args.switches.contains("--open"),
            );
        }
    }

    let mut workspace = args.flags.get("--workspace").cloned().unwrap_or_default();
    let mut pane = args.flags.get("--pane").cloned();
    if workspace.is_empty() || pane.is_none() {
        if let Ok(ports) = attribute_ports(&machine, &scan_listeners(&machine)?) {
            if let Some(item) = ports.iter().find(|p| p.port == remote_port) {
                if workspace.is_empty() {
                    workspace = item.workspace_label.clone();
                }
                if pane.is_none() {
                    pane = item.pane_id.clone();
                }
            }
        }
    }
    let forward = add_forward(
        &machine,
        remote_port,
        local_port,
        &workspace,
        pane.as_deref(),
    )?;
    report_add(
        &machine,
        &forward,
        json_out,
        args.switches.contains("--open"),
    )
}

fn report_add(machine: &Machine, forward: &Forward, json_out: bool, open: bool) -> Result<()> {
    let workspace = if forward.workspace_label.is_empty() {
        &machine.label
    } else {
        &forward.workspace_label
    };
    let url = workspace_url(workspace, forward.local_port);
    if open {
        open_browser(&url)?;
    }
    if json_out {
        let state = forward_state(forward)?;
        return print_json(&json!({"ok": true, "port": forward_json(forward, state)}));
    }
    println!(
        "Forwarded {}:{} -> {url}",
        machine.label, forward.remote_port
    );
    Ok(())
}

pub(crate) fn cmd_stop(args: &[String], json_out: bool) -> Result<()> {
    let args = parse_args(args, &[], &["--keep"])?;
    let [port] = args.positional.as_slice() else {
        return Err(err(
            "invalid_args",
            "usage: herdr-portal stop <local-port> [--keep] [--json]",
        ));
    };
    let local_port = parse_port(port)?;
    let keep = args.switches.contains("--keep");
    let forward = stop_forward(local_port, keep)?;
    if json_out {
        let mut row = forward_json(&forward, State::Inactive);
        row["saved"] = json!(keep);
        return print_json(&json!({"ok": true, "port": row}));
    }
    if keep {
        println!("Paused local port {local_port}; `start {local_port}` resumes it");
    } else {
        println!("Stopped local port {local_port}");
    }
    Ok(())
}

pub(crate) fn cmd_start(args: &[String], json_out: bool) -> Result<()> {
    let args = parse_args(args, &[], &[])?;
    let [port] = args.positional.as_slice() else {
        return Err(err(
            "invalid_args",
            "usage: herdr-portal start <local-port> [--json]",
        ));
    };
    let local_port = parse_port(port)?;
    let forward = load_forwards()?
        .into_iter()
        .find(|item| item.local_port == local_port)
        .ok_or_else(|| {
            err(
                "no_forward",
                format!("No tracked forward on local port {local_port}"),
            )
        })?;
    if forward.target.is_empty() {
        return Err(err(
            "invalid_args",
            "Local forwards have no tunnel to start",
        ));
    }
    if forward_alive(&forward)? {
        return Err(err(
            "not_inactive",
            format!("Local port {local_port} is already active"),
        ));
    }
    restore_one(&forward)?;
    let state = forward_state(&forward)?;
    if json_out {
        return print_json(&json!({"ok": true, "port": forward_json(&forward, state)}));
    }
    println!(
        "Started local port {local_port} -> {}",
        workspace_url(forward_workspace(&forward), local_port)
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn error_json_uses_api_code_and_falls_back_to_internal() {
        let coded = error_json(&err("no_forward", "No tracked forward on local port 9"));
        assert_eq!(coded["ok"], json!(false));
        assert_eq!(coded["error"]["code"], json!("no_forward"));
        let plain = error_json(&anyhow!("boom"));
        assert_eq!(plain["error"]["code"], json!("internal"));
    }

    #[test]
    fn parse_args_splits_flags_switches_and_positionals() {
        let args = parse_args(
            &strings(&["dell", "5173", "--local-port", "6000", "--open"]),
            &["--local-port"],
            &["--open"],
        )
        .unwrap();
        assert_eq!(args.positional, strings(&["dell", "5173"]));
        assert_eq!(
            args.flags.get("--local-port").map(String::as_str),
            Some("6000")
        );
        assert!(args.switches.contains("--open"));
    }

    #[test]
    fn parse_args_rejects_unknown_and_valueless_flags() {
        let unknown = parse_args(&strings(&["--bogus"]), &[], &[]).unwrap_err();
        assert_eq!(error_json(&unknown)["error"]["code"], json!("invalid_args"));
        assert!(parse_args(&strings(&["--state"]), &["--state"], &[]).is_err());
    }

    #[test]
    fn ports_and_states_validate() {
        assert_eq!(parse_port("5173").unwrap(), 5173);
        assert!(parse_port("0").is_err());
        assert!(parse_port("70000").is_err());
        assert_eq!(State::parse("inactive").unwrap(), State::Inactive);
        assert!(State::parse("up").is_err());
    }

    #[test]
    fn agent_selector_matches_pane_or_name() {
        let item = WorkspacePort {
            pane_id: Some("w3:p2".into()),
            agent_name: Some("reviewer".into()),
            ..Default::default()
        };
        assert!(matches_agent(&item, "w3:p2"));
        assert!(matches_agent(&item, "reviewer"));
        assert!(!matches_agent(&item, "w3:p1"));
        assert!(!matches_agent(&WorkspacePort::default(), "reviewer"));
    }

    #[test]
    fn scan_row_keeps_legacy_fields_and_reports_unforwarded_ports() {
        let machine = Machine {
            id: "m1".into(),
            label: "dell".into(),
            target: "lam@host".into(),
            session: "default".into(),
        };
        let item = WorkspacePort {
            workspace_id: "w3".into(),
            workspace_label: "web".into(),
            port: 5173,
            process: "node".into(),
            pane_id: Some("w3:p2".into()),
            pid: Some(42),
            ..Default::default()
        };
        let row = scan_json(&machine, &item, None, State::Available);
        assert_eq!(row["state"], json!("available"));
        assert_eq!(row["local_port"], Value::Null);
        assert_eq!(row["pane_id"], json!("w3:p2"));
        assert_eq!(row["port"], json!(5173));
        assert_eq!(row["status"], json!("○"));
        assert_eq!(row["forwarded"], json!(false));
    }
}
