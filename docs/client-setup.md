# Set up OpenCode on another machine

The plugin needs the Spool `url` and a source token. The token enables capture
and the archive search/read tools; both use the same HTTPS address.

## Install and configure

OpenCode installs the plugin from GitHub, pinned to a full commit hash, into its
own package cache. The machine needs Bun and Git credentials that can read this
private repository. A checkout is needed only to run the installer.

1. For a new independent OpenCode store, have the archive operator provision a
   source using [source add](ingestion.md#identity-and-credentials). Save its token
   in `~/.config/spool/source.token`, mode 0600, outside tracked configuration.
   For an existing installation, keep its token and checkpoints. Never copy
   another store's token or source name: each independent store needs its own
   credential and globally unique, stable name, even on the same machine.
2. From an up-to-date checkout of a commit that is on GitHub, run:

   ```sh
   bun run install-plugin --url https://spool.example.com \
     --token-file ~/.config/spool/source.token --source-alias work-laptop
   ```

   It writes one entry to `plugins` in the global `opencode.json`, preserving
   other configuration, and replaces any earlier Spool entry, including one that
   points at a checkout. Options already configured are kept unless a flag
   overrides them; the retired `readerUrl` and `sweepIntervalMs` are dropped.
   `--dry-run` prints the entry without writing it. The result looks like this:

```json
{
  "plugins": [{
    "package": "github:victhorio/spool#<full commit hash>::path:plugin",
    "options": {
      "url": "https://spool.example.com",
      "tokenFile": "~/.config/spool/source.token",
      "sourceAlias": "work-laptop"
    }
  }]
}
```

To update, pull and run `bun run install-plugin` again. Pushing to GitHub alone
changes nothing on a machine. The installer refuses uncommitted plugin changes
and commits that are not on GitHub, since OpenCode could not install them.

`sourceAlias` is optional and must match `^[a-z][a-z0-9-]{0,31}$`. Use the same
name as the operator assigned, or choose an unused name for an unnamed source.
The collector registers it once on startup, retrying transient failures; an
already assigned identical name is accepted. Names are stable: changing the
option cannot rename a source. A different assigned name (`source_alias_mismatch`)
or a name taken by another source (`source_alias_conflict`) stops capture until
the configuration is corrected and all instances are reloaded. Omitting the
option leaves any centrally assigned name unchanged; it does not clear it.
Adding an alias preserves checkpoints and does not recapture existing sessions.

Checkpoints are keyed by source, not `url`, so changing `url` does not recapture
sessions. Plugins installed from a checkout keyed them by both. When replacing
one, run the installer without `--url` first, let OpenCode reload so the plugin
moves the checkpoints, then run it again with `--url`; the installer refuses to
do both at once. Going back to such a plugin afterwards recaptures every session.

OpenCode reloads plugins when the global configuration changes, installing a new
package in the background first; `opencode plugin list` shows `spool` once it is
loaded. Restarting the managed OpenCode service while idle also reloads;
opening another TUI attached to an existing service does not. Coordinate a
restart rather than interrupting active sessions. All loaded Locations share
one collector per source and reject a differing configuration, including a
different `url`, until every Location has reloaded.

## Verify from that machine

Check network/API access separately from tool registration. This read-only probe
needs curl and jq. It sends the source token from a file, so the token does not
appear in the command line or shell history:

```bash
set -o pipefail
curl --fail --silent --show-error --max-time 5 \
  --header @<(printf 'Authorization: Bearer %s\n' "$(cat ~/.config/spool/source.token)") \
  --header 'Content-Type: application/json' --data '{"limit":1}' \
  https://spool.example.com/v1/agent/find | jq '{coverage, returned_sessions: (.sessions | length)}'
```

Expect a find response containing `coverage`. A 401 means the token is missing,
wrong or revoked; a timeout is a network or service availability problem.

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
status, an installation or direct HTTP success does not.

## Troubleshooting

| Symptom | Check |
| --- | --- |
| Tools and skill are absent | Check `opencode plugin list` and the OpenCode log for installation errors, such as missing GitHub access. If registered but unavailable to one agent, check that agent's permissions. |
| `unauthorized` | The source token is missing, wrong or revoked. Check `tokenFile` and run the probe above. |
| `reader_unavailable` | Run the probe from the affected machine and check the service's availability. |
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
identify the current host; remote ingestion requires HTTPS. The former
`sweepIntervalMs` option is accepted but ignored: capture is
event-driven, with explicit [repair](ingestion.md#capture-and-retention), not
periodic full sweeps.
