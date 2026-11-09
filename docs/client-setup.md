# Set up OpenCode on another machine

Spool has two independent connections. The public ingestion `url` and source
token enable capture. The private `readerUrl` enables archive search/read tools
and their skill. A plugin can be active and uploading successfully while exposing
no retrieval tools. Omitting `readerUrl` deliberately makes it capture-only.

## Install and configure

1. Clone or fast-forward this repository on the machine and run
   `bun install --frozen-lockfile` from its root. OpenCode loads the local
   TypeScript package directly; no Rust build or database is needed on a client.
2. For a new independent OpenCode store, have the archive operator provision a
   source using [source add](ingestion.md#identity-and-credentials). Save its token
   in `~/.config/spool/source.token`, mode 0600, outside tracked configuration.
   For an existing installation, keep its token and checkpoints. Never copy
   another store's token or source name: each independent store needs its own
   credential and globally unique, stable name, even on the same machine.
3. Merge this entry into `plugins` in `~/.config/opencode/opencode.json`, preserving
   other configuration. Use the machine's absolute checkout path, not the `.ts`
   entrypoint. Replace the ingestion URL with the operator-provided HTTPS URL.

```json
{
  "plugins": [{
    "package": "/absolute/path/to/spool/plugin",
    "options": {
      "url": "https://your-spool-host",
      "tokenFile": "~/.config/spool/source.token",
      "sourceAlias": "work-laptop",
      "readerUrl": "http://100.86.13.40:8090"
    }
  }]
}
```

`sourceAlias` is optional and must match `^[a-z][a-z0-9-]{0,31}$`. Use the same
name as the operator assigned, or choose an unused name for an unnamed source.
The collector registers it once on startup, retrying transient failures; an
already assigned identical name is accepted. Names are stable: changing the
option cannot rename a source. A different assigned name (`source_alias_mismatch`)
or a name taken by another source (`source_alias_conflict`) stops capture until
the configuration is corrected and all instances are reloaded. Omitting the
option leaves any centrally assigned name unchanged; it does not clear it.
Adding an alias preserves checkpoints and does not recapture existing sessions.

`http://100.86.13.40:8090` is the current private deployment's reader address;
use its current literal Tailscale IPv4 address if that node is replaced. Each
client needs `readerUrl` configured and Tailscale connected to the trusted tailnet.
Do not substitute the public ingestion URL, a MagicDNS name, HTTPS, or another
machine's `127.0.0.1`. Loopback works only when the reader runs on that same machine.
The source token authorizes ingestion, not reading; do not send it to the reader.
No new database, reader service or tailnet policy changes are needed on clients.

Pulling the checkout updates files, not already-loaded plugin instances. After
changing code or options, reload all Spool instances for that source. Restarting
the managed OpenCode service while idle is one way to do that; opening another
TUI attached to an existing service is not a reload. Coordinate a restart rather
than interrupting active sessions. Reload all loaded Locations together when
changing options, or their shared collector will reject conflicting configuration.

## Verify from that machine

Check network/API access separately from tool registration. These read-only
probes need curl and jq; they do not send credentials or read conversation bodies:

```bash
set -o pipefail
reader=http://100.86.13.40:8090
curl --noproxy '*' --fail --silent --show-error --max-time 5 \
  --output /dev/null --write-out 'HTTP %{http_code}\n' "$reader/healthz"
curl --noproxy '*' --fail --silent --show-error --max-time 5 \
  --header 'Content-Type: application/json' --data '{"limit":1}' \
  "$reader/api/agent/find" | jq '{coverage, returned_sessions: (.sessions | length)}'
```

Expect health **204** and a find response containing `coverage`. A successful
health probe alone does not establish that the agent API is deployed or granted
access to its database columns. A timeout is a network/reader availability problem,
not evidence of a bad source token.

After reload, check the intended agent's available tools, not just plugin status:

- `spool_find_sessions`, `spool_search_messages`, `spool_read_messages`
- Bundled skill `spool.searching-sessions` (name `searching-sessions`)

Use an agent allowed to use these tools, such as a permitted General agent.
Explore's restricted defaults are not broadened by the plugin; delegation does
not bypass permissions. Seeing no MCP resources does not diagnose this plugin:
Spool registers native OpenCode tools, not an MCP server.

Exercise `spool_find_sessions` with `{"limit":1}`. If the archive has a session,
pass its exact returned `session` reference to `spool_read_messages` with
`start: 1, end: 1`; then search a known phrase from that evidence with
`spool_search_messages`. References use `work-laptop/ses_…` when named, otherwise
`source-UUID/ses_…`; legacy UUID-qualified references remain valid. A bare native
OpenCode ID also works when globally unique. If `ambiguous_session` returns
`candidates`, select the qualified reference for the intended source, not simply
the first; `more: true` means additional candidates exist. Use find to narrow the
choice when needed. Only successful actual calls verify agent access; plugin
status, a Git pull or direct HTTP success does not.

## Troubleshooting

| Symptom | Check |
| --- | --- |
| Capture works but tools and skill are absent | `readerUrl` in the effective Spool options, then reload all instances. If registered but unavailable to one agent, check that agent's permissions. |
| `reader_unavailable` | Run the probes from the affected machine. Check Tailscale, the literal reader address and reader readiness; the public ingestion service does not serve these routes. |
| Capture logs `source_alias_mismatch` or `source_alias_conflict`, then `disabled_until_reload` | Keep the already assigned alias for this source, or choose an unused alias for an unnamed source. Correct the options and reload all instances; retrying cannot rename a source or take another source's name. |
| `ambiguous_session` | Select the intended source-qualified reference from `candidates`, or use find to narrow the choice. |
| `malformed_request` | Use the source-qualified reference from find and the documented arguments; a continuation request contains only its cursor. |
| `not_indexed` | The archive lacks complete projections. Update its source collector; `/spool-repair` can recapture sessions still present locally, not recover locally deleted sessions. |

## Optional settings

`repositoryRemote` selects a fetch remote instead of the default origin/sole-remote
selection. See [repository identity](retrieval.md#repository-identity).
Managed OpenCode service discovery is automatic and must resolve the plugin's own
process. For a nondefault registration use `serviceFile`; for an unmanaged server
set both `opencodeUrl` and `opencodePasswordFile`. Those endpoints must still
identify the current host; remote ingestion requires HTTPS, unlike private reader
HTTP. The former `sweepIntervalMs` option is accepted but ignored: capture is
event-driven, with explicit [repair](ingestion.md#capture-and-retention), not
periodic full sweeps.
