//! Opt-in auto-forward: `watch` forwards every remote listener that a pane in
//! an allowed workspace started, on the same port, the way VS Code does. Rules
//! live in `config.toml` under `[auto]`; see "Auto-forward" in README.md.

use super::*;

const MISSES_TO_STOP: u8 = 2;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    #[serde(default)]
    auto: AutoConfig,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct AutoConfig {
    #[serde(default)]
    pub enabled: bool,
    // Saved machine labels or ids; empty means every machine.
    #[serde(default)]
    machines: Vec<String>,
    // Workspace labels or ids; empty means every workspace.
    #[serde(default)]
    workspaces: Vec<String>,
    // Ports or "low-high" ranges; empty means every port from 1024.
    #[serde(default)]
    ports: Vec<PortRule>,
    #[serde(default)]
    skip: Vec<u16>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(untagged)]
enum PortRule {
    One(u16),
    Range(String),
}

impl PortRule {
    fn bounds(&self) -> Option<(u16, u16)> {
        match self {
            PortRule::One(port) => Some((*port, *port)),
            PortRule::Range(text) => {
                let (low, high) = text.split_once('-').unwrap_or((text, text));
                let low = low.trim().parse().ok()?;
                let high = high.trim().parse().ok()?;
                (low <= high).then_some((low, high))
            }
        }
    }
}

impl AutoConfig {
    fn allows_machine(&self, machine: &Machine) -> bool {
        self.machines.is_empty()
            || self
                .machines
                .iter()
                .any(|m| *m == machine.label || *m == machine.id)
    }

    fn allows_workspace(&self, id: &str, label: &str) -> bool {
        self.workspaces.is_empty() || self.workspaces.iter().any(|w| w == id || w == label)
    }

    fn allows_port(&self, port: u16) -> bool {
        port >= 1024
            && !self.skip.contains(&port)
            && (self.ports.is_empty()
                || self
                    .ports
                    .iter()
                    .filter_map(PortRule::bounds)
                    .any(|(low, high)| (low..=high).contains(&port)))
    }

    fn validate(&self) -> Result<()> {
        for rule in &self.ports {
            if rule.bounds().is_none() {
                return Err(api::err_with(
                    "invalid_args",
                    "config.toml: an [auto] ports entry is not a port or \"low-high\" range.",
                    format!(
                        "[auto] ports: '{}' is not a port or \"low-high\" range",
                        match rule {
                            PortRule::One(port) => port.to_string(),
                            PortRule::Range(text) => text.clone(),
                        }
                    ),
                ));
            }
        }
        Ok(())
    }
}

pub(crate) fn config_path() -> Result<PathBuf> {
    Ok(
        match env::var_os("HERDR_PLUGIN_CONFIG_DIR").filter(|d| !d.is_empty()) {
            Some(dir) => PathBuf::from(dir),
            None => home_dir()?
                .join(".config/herdr/plugins/config")
                .join(PLUGIN_ID),
        }
        .join(CONFIG_FILE),
    )
}

pub(crate) const CONFIG_FILE: &str = "config.toml";

pub(crate) fn load_config() -> Result<AutoConfig> {
    let path = config_path()?;
    let Ok(text) = fs::read_to_string(&path) else {
        return Ok(AutoConfig::default());
    };
    // toml's error quotes the file; the path names the home folder. Both stay out of JSON.
    let file: ConfigFile = toml::from_str(&text).map_err(|e| {
        api::err_with(
            "invalid_args",
            "config.toml does not parse.",
            format!("{}: {e}", path.display()),
        )
    })?;
    file.auto.validate()?;
    Ok(file.auto)
}

/// Counts of consecutive scans each auto forward's listener was missing, and
/// the last skip reason per port, kept across passes and `--once` runs.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct Memory {
    #[serde(default)]
    misses: HashMap<String, u8>,
    #[serde(default)]
    skipped: HashMap<String, String>,
}

fn memory_path() -> Result<PathBuf> {
    Ok(state_dir()?.join("auto.json"))
}

fn load_memory() -> Memory {
    memory_path()
        .ok()
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_memory(memory: &Memory) -> Result<()> {
    fs::write(memory_path()?, serde_json::to_string_pretty(memory)? + "\n")?;
    Ok(())
}

fn key(machine: &Machine, port: u16) -> String {
    format!("{}:{port}", machine.id)
}

/// Ports this pass should keep forwarded: pane-attributed listeners the rules allow.
fn candidates(config: &AutoConfig, ports: &[WorkspacePort]) -> Vec<WorkspacePort> {
    let mut seen = HashSet::new();
    ports
        .iter()
        .filter(|p| p.pane_id.is_some())
        .filter(|p| config.allows_workspace(&p.workspace_id, &p.workspace_label))
        .filter(|p| config.allows_port(p.port))
        .filter(|p| seen.insert(p.port))
        .cloned()
        .collect()
}

fn row(
    action: &str,
    port: u16,
    local: Option<u16>,
    item: Option<&WorkspacePort>,
    changed: bool,
) -> Value {
    json!({
        "action": action,
        "remote_port": port,
        "local_port": local,
        "status": if action == "auto-stop" { "stopped" } else if local.is_some() { "active" } else { "skipped" },
        "url": local.zip(item).map(|(l, i)| workspace_url(&i.workspace_label, l)),
        "pane_id": item.and_then(|i| i.pane_id.clone()),
        "workspace_id": item.map(|i| i.workspace_id.clone()),
        "agent": item.and_then(|i| i.agent.clone()),
        "changed": changed,
    })
}

/// One auto-forward pass for one machine. `requested` holds remote ports an
/// agent request currently wants; those are never stopped here.
pub(crate) fn tick_machine(
    machine: &Machine,
    config: &AutoConfig,
    requested: &HashSet<u16>,
) -> Result<Vec<Value>> {
    let mut memory = load_memory();
    let mut actions = Vec::new();
    let wanted = if config.enabled && config.allows_machine(machine) {
        candidates(
            config,
            &attribute_ports(machine, &scan_listeners(machine)?)?,
        )
    } else {
        Vec::new()
    };
    let wanted_ports: HashSet<u16> = wanted.iter().map(|p| p.port).collect();

    for item in &wanted {
        let k = key(machine, item.port);
        memory.misses.remove(&k);
        let outcome = match existing_forward(machine, item.port)? {
            Some(existing) if forward_alive(&existing)? => continue,
            Some(existing) if existing.origin == Origin::Auto => {
                restore_one(&existing).map(|()| (existing.local_port, "auto-resume"))
            }
            // A saved manual or request forward that is down is not auto's to reopen.
            Some(_) => continue,
            None => add_forward(
                machine,
                item.port,
                Some(item.port),
                &item.workspace_label,
                item.pane_id.as_deref(),
            )
            .and_then(|f| {
                requests::mark_origin(machine, f.local_port, Origin::Auto)?;
                Ok((f.local_port, "auto-add"))
            }),
        };
        match outcome {
            Ok((local, how)) => {
                memory.skipped.remove(&k);
                actions.push(row(how, item.port, Some(local), Some(item), true));
            }
            Err(error) => {
                let code = requests::error_code(&error);
                let changed = memory.skipped.get(&k) != Some(&code);
                memory.skipped.insert(k, code.clone());
                let mut r = row("auto-skip", item.port, None, Some(item), changed);
                r["error"] = json!(code);
                actions.push(r);
            }
        }
    }

    for forward in load_forwards()? {
        if forward.origin != Origin::Auto
            || !requests::same_machine(&forward, machine)
            || wanted_ports.contains(&forward.remote_port)
            || requested.contains(&forward.remote_port)
        {
            continue;
        }
        let k = key(machine, forward.remote_port);
        let misses = memory.misses.entry(k.clone()).or_insert(0);
        *misses += 1;
        if *misses < MISSES_TO_STOP {
            continue;
        }
        memory.misses.remove(&k);
        let stopped = stop_forward(forward.local_port, false).is_ok();
        actions.push(row(
            if stopped { "auto-stop" } else { "error" },
            forward.remote_port,
            Some(forward.local_port),
            None,
            stopped,
        ));
    }
    memory.skipped.retain(|k, _| match k.rsplit_once(':') {
        Some((id, p)) if id == machine.id => p.parse().is_ok_and(|p| wanted_ports.contains(&p)),
        _ => true,
    });
    save_memory(&memory)?;
    Ok(actions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> AutoConfig {
        let file: ConfigFile = toml::from_str(text).unwrap();
        file.auto.validate().unwrap();
        file.auto
    }

    fn port(port: u16, pane: Option<&str>, ws: &str) -> WorkspacePort {
        WorkspacePort {
            workspace_id: ws.into(),
            workspace_label: format!("{ws}-label"),
            port,
            pane_id: pane.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn missing_section_is_disabled() {
        assert!(!parse("").enabled);
        assert!(!parse("[auto]\n").enabled);
    }

    #[test]
    fn rules_filter_ports_workspaces_and_machines() {
        let c = parse(
            r#"
[auto]
enabled = true
machines = ["dell"]
workspaces = ["monitors", "w9"]
ports = [10350, "3000-4999"]
skip = [4200]
"#,
        );
        assert!(c.allows_port(10350) && c.allows_port(3102) && c.allows_port(4999));
        assert!(!c.allows_port(4200) && !c.allows_port(5000) && !c.allows_port(80));
        assert!(c.allows_workspace("w1", "monitors") && c.allows_workspace("w9", "x"));
        assert!(!c.allows_workspace("w2", "web"));
        let dell = Machine {
            id: "m1".into(),
            label: "dell".into(),
            target: "t".into(),
            session: "default".into(),
        };
        let other = Machine {
            label: "mini".into(),
            id: "m2".into(),
            ..dell.clone()
        };
        assert!(c.allows_machine(&dell) && !c.allows_machine(&other));
    }

    #[test]
    fn empty_rules_allow_everything_from_1024() {
        let c = parse("[auto]\nenabled = true\n");
        assert!(c.allows_port(1024) && c.allows_port(65535) && !c.allows_port(1023));
        assert!(c.allows_workspace("any", "thing"));
    }

    #[test]
    fn bad_config_is_rejected() {
        let file: ConfigFile = toml::from_str("[auto]\nports = [\"9-1\"]\n").unwrap();
        assert!(file.auto.validate().is_err());
        assert!(toml::from_str::<ConfigFile>("[auto]\nenable = true\n").is_err());
    }

    #[test]
    fn only_pane_attributed_allowed_ports_are_candidates() {
        let c = parse("[auto]\nenabled = true\nworkspaces = [\"w1\"]\n");
        let ports = vec![
            port(5173, Some("w1:p1"), "w1"),
            port(8080, None, "w1"),
            port(9000, Some("w2:p1"), "w2"),
            port(80, Some("w1:p2"), "w1"),
            port(5173, Some("w1:p3"), "w1"),
        ];
        let picked: Vec<u16> = candidates(&c, &ports).iter().map(|p| p.port).collect();
        assert_eq!(picked, vec![5173]);
    }
}
