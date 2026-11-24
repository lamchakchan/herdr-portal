# Herdr Portal

A [Herdr](https://herdr.dev) plugin. Forward a listener from a remote Herdr
workspace onto this laptop as:

```text
http://herdr.{workspace}.localhost:{port}
```

Plugin id: `lamchakchan.herdr-portal`  
Binary: `herdr-portal` (Rust)

It uses native saved machines (`herdr machine add`). It does not wrap
`herdr --remote`.

Herdr Portal started as a copy of [randomradio/herdr-ports](https://github.com/randomradio/herdr-ports)
(MIT), with its full history kept. On top of that it adds a JSON API for agents
and scripts (see [API](#api)).

![Forward a workmbp loopback port](docs/demo.gif)

The recording is the upstream demo, made before the rename, so it shows the
`herdr-ports` command. It is a real session: a loopback service on `workmbp` (Tailscale
`chenyangzhaos-macbook-pro`) fails on this laptop until `herdr-ports add`
opens SSH `-L`. [MP4](docs/demo.mp4) · [asciicast](docs/demo.cast) ·
re-record with `docs/record-session.sh`.

Version 0.4.0 adds optional Cloudflare Quick Tunnels for temporary public HTTPS
sharing. The popup keeps cached lists, separate forwarding and browser actions,
three-state status indicators, and discovery of detached workspace servers.

## Persist

Tunnels are SSH `-L` forwards on a ControlMaster that:

- lives under `/tmp/hp-<hash>` (short enough for macOS Unix sockets)
- starts in a new session with SIGHUP ignored, so closing the manager pane
  does not kill the mux
- uses `ControlPersist=yes`
- is recorded in plugin state
- is restored by `./herdr-portal restore` on Herdr startup

Closing the popup leaves its forwards running. A forward stops when you toggle
it off, run `stop`, reboot, or lose its SSH connection. Saved forwards can be
resumed with Space or `restore`.

## UI

The manager opens in a **popup** without changing your pane layout.

1. Select **Local** in the sidebar (the bind happens on this laptop).
2. Run the pick action.
3. Choose a machine, then a workspace.
4. A row shows `○` (closed), `◐` (closed but saved), or `●` (saved and alive).
   Click a row to select it. Press Space to start, resume, or stop its forward.
   Enter or `o` opens its URL, including for local listeners.
5. To share a service publicly, press `t`, then `y` to confirm. Escape stops
   public sharing without stopping the SSH forward.

Use arrow keys (or j/k) and the mouse wheel to select rows. Press Enter or `o` to open
the selected URL, `r` to refresh, and Escape or `q` to go back. Escape or `q`
at the machine list closes the popup. Port conflicts open a local-port prompt.

Alive means the SSH master is running and owns the local forwarding listener.
It does not confirm that the remote application responds. Saved forwards remain
visible after their tunnel stops. `stop <local-port>` also removes an inactive
saved forward. See [API](#api) for JSON output.

The popup keeps errors visible and leaves active forwards running when closed.
Machine, workspace, and port lists are cached while the popup is open. Going
back, selecting a row, or opening a URL does not rescan. Space updates only the
selected port's status. Press `r` to refresh external changes; a failed refresh
keeps the previous port list visible. Refreshing the workspace list returns to
the machine list; select the machine again to load its workspaces. Displayed URLs use
`http://herdr.{workspace}.localhost:{local-port}`, including remapped ports.

The hostname is a browser address for a loopback-only SSH forward. It does not
provide HTTPS or separate port namespaces: two workspaces cannot use the same
local port at once. If a port is occupied, choose another in the popup. The
remote service must accept the workspace hostname in its HTTP Host header.

## Public sharing with Cloudflare

Select a port and press `t` to open the sharing view. Press `y` to confirm
public access and start a Cloudflare Quick Tunnel. For a remote port, first
press Space in the port list to enable its SSH forward. Local ports work directly.

The view shows a temporary `https://….trycloudflare.com` URL. Enter or `o`
opens that public URL. Escape or `q` stops the public tunnel and returns to
the cached port list. The SSH forward stays active. Public tunnels are not
saved or restored; keep the sharing view open while using the link.

**Anyone with the URL can access the service.** The plugin adds no authentication.
Share only services and data that you intend to make public.

Install `cloudflared` on the host running the popup (on macOS:
`brew install cloudflared`). No Cloudflare account is needed for Quick Tunnels.
The plugin connects to `http://127.0.0.1:{local-port}` and sets the origin Host
header to `herdr.{workspace}.localhost:{local-port}`. Your application must accept
that header. The existing local workspace URL does not change.

Quick Tunnels are for testing, not production: their URLs are temporary,
they have a 200 concurrent-request limit, and they do not support SSE.
A `.cloudflared/config.yaml` can prevent Quick Tunnels from working; the plugin
does not change your Cloudflare configuration. Named tunnels, custom public
domains, and Cloudflare Access configuration are not included.
See [Cloudflare's Quick Tunnel documentation](https://developers.cloudflare.com/cloudflare-one/networks/connectors/cloudflare-tunnel/do-more-with-tunnels/trycloudflare/).

## Discovery

The plugin scans TCP listeners and first matches their process ancestry to
Herdr panes. For detached services, it checks the process working directory
against pane directories. It also recognizes an absolute `--directory` or `-d`
argument from `python -m http.server`, which can serve a project while running
from another directory. The most specific matching directory wins; ambiguous
matches are omitted.

Listeners without enough ownership evidence are not assigned to a workspace.
Use `add <machine> <port> --workspace NAME` to forward a known service manually.
Forwarding currently targets IPv4 loopback on the remote host; IPv6-only or
interface-only services need a compatible remote listener.

## Install

Needs `cargo` on the machine that installs the plugin.

```bash
herdr plugin install lamchakchan/herdr-portal --yes
herdr server reload-config
```

Run the same commands to update a GitHub installation. For a local development
checkout, build and link it instead:

```bash
cargo build --release
cp target/release/herdr-portal herdr-portal
herdr plugin link "$PWD" --enabled
herdr server reload-config
```

Close and reopen an existing popup after an update. To switch a local link back
to a GitHub installation, first run `herdr plugin unlink lamchakchan.herdr-portal`.

## Keybinding

```toml
[[keys.command]]
key = "prefix+shift+p"
type = "plugin_action"
command = "lamchakchan.herdr-portal.pick"
description = "pick remote port"
```

Reload config after install (global menu → `reload config`).

## CLI

After install, `herdr-portal` is in the plugin root.

```bash
./herdr-portal scan workbox
./herdr-portal scan workbox --json
./herdr-portal pick workbox
./herdr-portal add workbox 8765 --open
./herdr-portal add workbox 8765 --local-port 18765 --workspace mo
./herdr-portal list
./herdr-portal stop 8765
./herdr-portal stop 8765 --keep    # pause: stays saved as inactive
./herdr-portal start 8765          # resume an inactive forward
./herdr-portal restore
```

Run outside herdr, the CLI uses the same state folder that herdr hands the plugin
(`${XDG_STATE_HOME:-~/.local/state}/herdr/plugins/lamchakchan.herdr-portal`).
Forwards added from a shell therefore show in the popup and come back with the
startup `restore`. If `herdr` on a remote machine isn't on the PATH of a
non-interactive SSH shell, set `HERDR_PORTAL_REMOTE_HERDR` to its full path.

## API

The API for agents and scripts is the CLI with `--json`. It doesn't run a server.
`scan`, `list`, `add`, `stop` and `start` each print one JSON object on stdout.

- Success: `{"ok": true, ...}`, exit 0.
- Failure: `{"ok": false, "error": {"code": "...", "message": "..."}}`, exit 1.

| Task | Command |
| --- | --- |
| Ports per agent | `scan <machine> --json --agent <pane-id or agent name>` (also `--pane`, `--workspace`) |
| All active forwards | `list --json --state active` |
| All inactive forwards | `list --json --state inactive` |
| Remote ports not forwarded | `scan <machine> --json --state available` |
| Agent progress and teams | `progress <machine> --json` |
| Record a work-history event here | `event <kind> --work <id> [--field value …] [--json]` |
| Read a machine's work history | `history <machine> --json [--since <epoch s>]` |
| Open a forward | `add <machine> <port> [--local-port N] --json` |
| Close a forward | `stop <local-port> --json` (add `--keep` to pause it instead) |
| Resume a paused forward | `start <local-port> --json` |

Ports come back as `{"ok": true, "ports": [...]}`; `add`, `stop` and `start`
return `{"ok": true, "port": {...}}`. Every port has this shape:

```json
{
  "machine": "dell", "machine_id": "5b4e…",
  "remote_port": 5173, "local_port": 5173,
  "state": "active",
  "url": "http://herdr.web.localhost:5173",
  "workspace": "web", "workspace_id": "w3", "tab_id": "w3:t1", "pane_id": "w3:p2",
  "origin": "manual",
  "agent": "claude", "agent_name": null,
  "process": "node", "pid": 4242
}
```

| `state` | Meaning |
| --- | --- |
| `active` | Saved, and the SSH master owns the local listener (●). |
| `inactive` | Saved but not running (◐): paused with `stop --keep`, or lost after a reboot or SSH drop. |
| `available` | Listening on the remote machine but not forwarded (○). Only `scan` reports it. |

`local_port` is null while a port is `available`. The pane fields are set when
process ancestry ties the listener to a pane. They're null for detached servers
matched by directory, and `list` reports only the `pane_id` that `add` saved.
`scan` rows also keep the fields of the first JSON format: `port`, `saved`,
`forwarded` and `status`. `origin` says who opened a saved forward: `manual`
(`add`, the picker or restore), `request` (an agent request) or `auto`; it is
null for a port that isn't saved. Asking `add` for a forward that is already active
returns it unchanged.

With `--json`, an error's `message` is fixed text for SSH and remote-command failures. It never quotes the SSH
target, or ssh's or a remote command's stderr, which can name the user, the host or a home path. Without `--json`, the
CLI prints ssh's own words, so a person at the terminal can see why a connection failed.

| Error code | When |
| --- | --- |
| `invalid_args` | Unknown command, flag or port. |
| `machine_not_found` | No saved machine matches the selector. |
| `local_port_in_use` | Another process holds the local port; retry with `--local-port`. |
| `no_forward` | No tracked forward on that local port. |
| `not_inactive` | `start` on a forward that is already active. |
| `ssh_failed` | SSH could not connect, log in, or open or stop a forward. |
| `herdr_failed` | `herdr` on the target machine failed or wasn't found. |
| `internal` | Anything else. |

`watch --status --json` and `start-watch --json` give `log` and `auto.config` as file names (`watch.log`,
`config.toml`), relative to the plugin's state and config folders, so the JSON names no home path. Without `--json`,
`start-watch` prints the full log path.

### Changes since 0.5.0: errors in `--json`

| Before | Now |
| --- | --- |
| An SSH failure's `message` quoted ssh's stderr and the target. | Fixed text; the plain CLI keeps ssh's words. |
| `watch --once --json` rows (`error`, `auto_error`) quoted the target and ssh's stderr. | Fixed text; `watch.log` and plain `watch` keep the detail. |
| Exit 255 from a remote `herdr` call gave `herdr_failed`; from a remote port scan, `internal`. | Both give `ssh_failed`: 255 is ssh's own exit code. |
| A missing local `herdr` gave `internal` with its path. | `herdr_failed`, "Could not run herdr." A missing `ssh` gives `ssh_failed`. |
| `machine_not_found` quoted the selector. | Fixed text, no selector. |
| An ambiguous selector gave `internal` and quoted it. | `invalid_args`, fixed text. |
| A `config.toml` that doesn't parse put its path and a snippet in `auto.error`. | "config.toml does not parse." (`invalid_args`). |
| `watch --status` and `start-watch` gave absolute `log` and `config` paths. | File names, as above. |
| `restore`, `pick`, `open-picker`, `from-url`, `help` and loop-mode `watch` ignored `--json`. | `invalid_args`: they have no JSON output. |

## Agent progress

`progress <machine> --json` reads two things on that machine: the herdr-projects progress records
(`~/.herdr-projects/.progress/*.json`) and the Claude Code team configs (`~/.claude/teams/*/config.json`).
`Local` reads this machine. It also stats each teammate's transcript without reading it. Headway uses this to show a
remote agent's progress and teammates.

```json
{
  "ok": true, "schema": 1,
  "machine": "dell", "machine_id": "5b4e…",
  "now": 1764000000,
  "progress": [
    {"file": "w3_p2-1a2b3c4d.json", "record": {"pane_id": "w3:p2", "terminal_id": "…", "percent": 55,
      "activity": "…", "reported_at": 1763999900, "session_started_at": 1763990000}},
    {"file": "w4_p1-9f8e7d6c.json", "error": "bad_json"}
  ],
  "teams": [
    {"team": "squad", "config": {"leadSessionId": "…", "members": [{"name": "impl-x", "agentType": "…", "joinedAt": 1763990000000}]},
     "transcripts": [{"name": "agent-aimpl-x-0123456789abcdef.jsonl", "mtime_ms": 1763999950000}],
     "transcripts_complete": true}
  ],
  "truncated": false
}
```

- `now` is the machine's own clock, in epoch seconds, so a caller can judge staleness without clock skew.
- A record keeps only the six keys shown. A team keeps only `leadSessionId` and each member's `name`, `agentType` and
  `joinedAt`. Every other key, a member's `prompt` first of all, is dropped before printing.
- A kept value is a string or a number, as its key expects (`joinedAt` may be either), or it is dropped. Strings lose
  control and format characters and are cut to 256 characters. Numbers pass unchanged, whatever their size or sign, so
  a consumer must clamp them (a percent to 0–100, a time to a sane range).
- `transcripts` lists the lead's teammate transcripts newest first, at most 1000 per lead. `transcripts_complete` is
  false when that list was cut, or the lead's folder wasn't read, or `stat` failed for any transcript: a teammate
  missing from it has an unknown state, not "no transcript yet". A transcript whose name isn't plain, or whose path holds a newline or tab, is never listed, and it makes the list incomplete.
  The lead is the config's top-level `leadSessionId` only.
- Nothing is logged or written.
- An entry's `error` is `bad_json`, or `too_large` past 256 KiB.
- A missing folder gives `[]`.
- `truncated` is true when a cap stopped the read: 500 records, 100 teams, 1000 transcripts for one lead or 5000 in
  all, or 1 MiB each of records and of team configs. A file past 256 KiB sends no content, only its name, and counts toward neither budget.
- One fixed script runs per call, over the machine's SSH master. It interpolates nothing, and a lead id becomes part
  of a path only once it matches `^[0-9a-f-]{36}$`.
- The read has 20 s and 4 MiB of output. Past either, the portal kills ssh, or the local script, with everything it
  started, and fails with `internal`.
- An older portal answers `invalid_args` for this command.

## Work history

`event <kind> --work <id> …` appends one line to this machine's work history, which Headway's Agents › History view
reads. Scripts call it at fixed points: when work starts, when an agent joins, after a review verdict, a fix round, a
full CI run, a PR change, and when work ends. Agents never write the file by hand.

```sh
herdr-portal event start  --work AT-3400 --name impl-filter --role builder --model claude-opus-5-5 --effort high \
                          --pane w1:p2 --terminal term-9
herdr-portal event review --work AT-3400 --verdict changes --reviewer independent --name review-1 --model opus
herdr-portal event fix    --work AT-3400 --round 1
herdr-portal event ci     --work AT-3400 --result pass --head 7ee0e8ad
herdr-portal event pr     --work AT-3400 --repo glydways/headway --number 298 --action merged
herdr-portal event end    --work AT-3400 --outcome done
```

- **Where:** `<state>/history/<yyyy-mm>.jsonl`, one file per UTC month, append-only. The directory is 0700 and each
  file 0600. `<state>` is herdr's plugin state directory, or `${XDG_STATE_HOME:-~/.local/state}/herdr/plugins/
  lamchakchan.herdr-portal` when herdr doesn't set one. A machine read over SSH is read at that default only, so a
  custom state directory is local-only.
- **A line** is one JSON object: `schema` (1), `at` (epoch seconds, set by the portal; the caller can't set it),
  `kind`, `work`, and the kind's fields. There is no free-text field: nothing from a prompt, a ticket or an agent's
  words can be written.

| Kind | Needs | May add |
| --- | --- | --- |
| `start`, `agent` | `--name`, `--role`, `--model` | `--effort`, `--pane`, `--terminal` |
| `review` | `--verdict`, `--reviewer`, `--name`, `--model` | |
| `fix` | `--round` | |
| `ci` | `--result`, `--head` | |
| `pr` | `--repo`, `--number`, `--action` | |
| `end` | `--outcome` | |

| Field | Values |
| --- | --- |
| `work` | A Jira key (`AT-3400`; a project key of at most 10), `repo:branch` (`herdr-portal:feat/x`; `/` only between branch segments, none `.`, `..` or starting with `-`; at most 128 bytes), or a tracker id (`q-sticky-agents`, `1152`; at most 64) |
| `name`, `terminal` | `[A-Za-z0-9][A-Za-z0-9._-]*`, at most 64 |
| `model` | `[a-z0-9][a-z0-9.-]*`, at most 64 (`claude-opus-5-5`) |
| `pane` | `<id>:<id>` (`w1:p2`) |
| `role` | `builder`, `reviewer`, `designer`, `fixer`, `lead` |
| `effort` | `low`, `medium`, `high`, `xhigh`, `max`: only what the starter stated, never a guess |
| `verdict` | `approve`, `block`, `changes` |
| `reviewer` | `self`, `independent` |
| `round` | 1 to 1000 |
| `result` | `pass`, `fail` |
| `head` | 7 to 40 lower-case hex digits |
| `repo` | `owner/name` |
| `number` | 1 to 1,000,000,000 |
| `action` | `opened`, `merged`, `closed`; for a merge queue, `queued` (submitted), `requeued` (back to waiting after a failure or a push) and `dequeued` (taken out without merging) |
| `outcome` | `done`, `dropped` |

- A field the kind doesn't take, a missing one, a flag given twice, an unknown flag (`--at` among them) or any value
  outside its pattern fails with `invalid_args`, and nothing is written. The message names the field, never the value;
  for an unknown `--flag=value` it names only `--flag`, and an unknown argument that isn't a plain `--name` is not
  named at all. Numbers are plain digits with no sign or leading zero.
- The file is opened without following a symlink and without blocking. It must be a regular file this user owns with
  no other hard link, and is made 0600 through the open file. The `history` directory is opened without following a
  symlink and chmodded through that open directory, so a symlinked or non-directory `history` is refused. These fail
  with `internal`, as does a clock outside 2020 to 2100 (the portal sets the time, so that is the machine's fault).
- Each line, at most 1 KiB, goes in one `write(2)` on an append-only file, so writers running at once never
  interleave. A write cut short fails with `internal` and is left as it is; the reader drops a partial line.
- Exit 0 and no output on success. With `--json`, `{"ok": true, "event": {…}}`.
- **Slugs can carry words.** A name (64), a branch (128) or a repo name (100) may spell out words, as branch names
  often do. The log never holds prose fields, but those ids are whatever the caller named things.
- **Schema changes.** A new field or kind needs `schema` 2. Readers of schema 1 drop those lines and report
  `complete: false`, so old Headways show less, never something wrong.
- **Callers skip** when the binary is missing (exit 127) or answers `invalid_args` for `event`, as a portal older than
  the command does. A missing history must never stop the work.

### Reading it

`history <machine> --json [--since <epoch s>]` returns the events at or after `since`, oldest first. `Local` reads
this machine's files directly. Any other machine is read over its SSH master by one fixed script, which writes nothing
there.

```json
{"ok": true, "schema": 1, "machine": "devbox", "now": 1796000000,
 "events": [{"schema": 1, "at": 1795990000, "kind": "fix", "work": "AT-3400", "round": 1}],
 "complete": true}
```

- `since` defaults to 7 days before now and may be at most 400 days back. It is plain digits, like an event's
  numbers. Anything else fails with `invalid_args`.
- `machine` is the machine's label, as the caller passed it, never the SSH target. A line holds no machine of its
  own.
- `now` is the read machine's clock, in epoch seconds.
- The read starts at the month of `since` minus one day, so a clock a little behind still finds its lines.
- **`complete` is false** when anything was left out:
  - a line that isn't a valid schema 1 event, from a newer writer or a damaged file, is dropped;
  - two lines merged by a cut write are dropped, and the lines after them are still read;
  - more than 2 MiB of files: the newest months are read first, and the oldest file read loses its front;
  - more than 10,000 events: the newest are kept;
  - a month-named entry that is a link, a FIFO, a directory or anything but a regular file is never read.
- A `history` directory that is a link fails with `internal`, as it does for `event`.
- Over SSH, the script reads the default state directory only. It doesn't see `HERDR_PLUGIN_STATE_DIR`, which herdr
  sets only for its own plugin runs, so a machine whose herdr uses another state directory reads as having no history.
  `progress` has the same limit.
- A machine with no history gives no events and `complete: true`.
- The read has 20 s and 4 MiB of output. Past either, the portal kills ssh and fails with `internal`. An SSH failure
  is `ssh_failed`, with fixed text.
- An older portal answers `invalid_args` for this command, and callers skip then, as for `event`.

## Agent requests

An agent on the remote machine can ask the laptop for a forward without any
access to the laptop. It sets a token on its own Herdr pane; `watch` on the
laptop reads every saved machine's panes, opens the forward, and answers on the
same pane. Nothing has to be installed on the remote machine.

On the remote machine, inside a Herdr pane:

```sh
herdr-portal request 5173 --wait 10 --json   # same local port
herdr-portal request 5173 --any --json       # any free local port
herdr-portal request 5173 --local-port 6000 --json
herdr-portal requests --json                 # this pane; --all for every pane
herdr-portal release 5173 --json
```

`request` returns `{"ok": true, "request": {...}}` with `status` `pending`
until the laptop answers, then `active` with `local_port`, `url` and
`localhost_url`. With `--wait SECS` it waits for that answer. If the laptop
can't forward the port, the request fails with the laptop's error code, such as
`local_port_in_use`. Ports below 1024 are refused.

Without the plugin installed there, the same request is two Herdr calls:

```sh
herdr pane report-metadata "$HERDR_PANE_ID" --source herdr-portal --token portal_req_5173=same@1
herdr pane get "$HERDR_PANE_ID"   # .result.pane.tokens.portal_st_5173 is active:5173@1
herdr pane report-metadata "$HERDR_PANE_ID" --source herdr-portal --clear-token portal_req_5173
```

| Token | Written by | Value |
| --- | --- | --- |
| `portal_req_<port>` | the agent, source `herdr-portal` | `same`, `any` or a local port, then `@<nonce>` |
| `portal_st_<port>` | `watch`, source `herdr-portal-client` | `pending`, `active:<local-port>` or `error:<code>`, then the request's `@<nonce>` |

The nonce is `[0-9a-z]`; an answer counts only when its nonce matches the
request, so a new request never reads an old answer.

On the laptop, `herdr-portal watch --once --json` runs one pass over every
saved machine. It opens forwards for new requests, resumes saved ones, and
reuses a forward already open on that remote port, including one opened by
hand. When the request or its pane goes away, it closes only forwards it opened
itself. A pane closing therefore takes its forwards with it. A request that
failed is tried again on every pass, with its answer token left as it is, so
freeing the local port lets the next pass open it.

`watch` without `--once` keeps doing that every 2 s (`--interval MS`). Herdr
starts it as the plugin's startup command, `watch --restore-first`, which first
reopens saved forwards. Only one runs at a time, held by a lock in the state
directory; a second one exits at once. When the plugin is updated, the running
`watch` switches to the new binary within one pass, without a Herdr restart.
Herdr runs startup commands only when its server starts, so after installing
the plugin, start it once with `herdr-portal start-watch` (or the "Start the
request watcher" action). `watch --status --json` reports whether it runs, its
pid and its log, `watch.log` in the state directory. Herdr doesn't restart a
watcher that crashed or was killed until its server next starts; run
`start-watch` again.


## Auto-forward

Off by default. With it on, `watch` forwards every remote listener that a
Herdr pane started, as VS Code does, on the same port number. Rules live in
`config.toml` in the plugin's config directory
(`~/.config/herdr/plugins/config/lamchakchan.herdr-portal/`, or
`$HERDR_PLUGIN_CONFIG_DIR`), and each pass reads them again:

```toml
[auto]
enabled = true
machines = ["dell-pro-max"]      # saved machine labels or ids; empty = all
workspaces = ["monitors"]        # workspace labels or ids; empty = all
ports = [10350, "3000-4999"]     # ports or "low-high" ranges; empty = 1024 and up
skip = [4200]                    # never auto-forward these
```

- Only listeners tied to a pane through process ancestry count; detached
  servers and ports below 1024 never do.
- A port is never renumbered. If the local port is taken, the row is
  `auto-skip` with the error code, logged once, and tried again on later passes.
- Auto runs every 5 s, after the request pass. It never touches forwards
  opened by hand or by a request, and a port an agent has requested is never
  auto-stopped.
- An auto forward closes after its listener has been gone for two scans in a
  row, or once the rules stop allowing it, including `enabled = false`.
- If `config.toml` doesn't parse, auto does nothing until it's fixed, and
  forwards it already opened stay open. `watch --status --json` shows whether
  auto is on, the config path and the error; the log has the error once, and
  `auto config: ok again` when it clears.
- Log lines start with the UTC time of day.

## Requirements

- Herdr 0.9.0 or newer
- macOS or Linux
- Rust/`cargo` to build
- OpenSSH with key auth: `ssh -o BatchMode=yes <target> true`
- `lsof` or `ss` on each scanned host
- `lsof` or `ss` on the local host to verify forwarding status
- Optional: `cloudflared` on the popup host for public sharing

## Marketplace

Public GitHub repository with topic `herdr-plugin`. Herdr indexes that topic
about every 30 minutes: https://herdr.dev/plugins/

Install from GitHub:

```bash
herdr plugin install lamchakchan/herdr-portal --yes
```
