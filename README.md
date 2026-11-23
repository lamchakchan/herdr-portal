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

| Error code | When |
| --- | --- |
| `invalid_args` | Unknown command, flag or port. |
| `machine_not_found` | No saved machine matches the selector. |
| `local_port_in_use` | Another process holds the local port; retry with `--local-port`. |
| `no_forward` | No tracked forward on that local port. |
| `not_inactive` | `start` on a forward that is already active. |
| `ssh_failed` | An SSH control command failed. |
| `herdr_failed` | `herdr` on the target machine failed or wasn't found. |
| `internal` | Anything else. |

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
pid and its log, `watch.log` in the state directory.


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
