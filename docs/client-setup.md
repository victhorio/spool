# Set up OpenCode on a machine

The plugin needs the Spool `url` and a source token. It captures sessions and
provides the archive's agent tools.

## Install

OpenCode installs the plugin itself from GitHub, pinned to a full commit hash,
so the machine needs Bun and Git credentials that can read this private
repository. A checkout is needed only to run the installer. From an up-to-date
checkout of a commit that is on GitHub, run:

```sh
bun run install-plugin --url https://spool.example.com --source-alias work-laptop
```

Without a token file (by default `~/.config/spool/source.token`), the installer
runs [device login](ingestion.md#device-login): it prints the reader's `/device`
address and a code. Open the address, type the code, check that the page shows
this machine's alias, and approve. Each independent OpenCode store needs its own
login and alias, even on the same machine; never copy another store's token. An
existing installation keeps its token file and checkpoints.

The installer writes one Spool entry to `plugins` in the global `opencode.json`,
replacing any earlier one and keeping other configuration and existing options
unless a flag overrides them; `--dry-run` prints it instead. It refuses
uncommitted plugin changes and commits that aren't on GitHub, since OpenCode
couldn't install them. To update, pull and run it again: pushing to GitHub alone
changes nothing on a machine.

`sourceAlias` must match `^[a-z][a-z0-9-]{0,31}$` and is permanent: use the
alias already assigned to this source, or an unused one for an unnamed source.
Omitting it leaves any assigned alias in place. Checkpoints are keyed by source,
not `url`, so changing `url` or adding an alias doesn't recapture sessions.

OpenCode reloads plugins when the global configuration changes, installing the
new package in the background first; restarting the managed OpenCode service
while idle also reloads, but opening another TUI attached to it doesn't. All
loaded Locations share one collector, which rejects a differing configuration
until every Location has reloaded, so coordinate a restart rather than
interrupting active sessions.

## Verify

This read-only probe checks network and token, sending the token from its file
so it stays out of shell history:

```bash
set -o pipefail
curl --fail --silent --show-error --max-time 5 \
  --header @<(printf 'Authorization: Bearer %s\n' "$(cat ~/.config/spool/source.token)") \
  --header 'Content-Type: application/json' --data '{"limit":1}' \
  https://spool.example.com/v1/agent/find | jq '{coverage, returned_sessions: (.sessions | length)}'
```

A 401 means the token is missing, wrong or revoked. Then, in OpenCode, have an
agent that is allowed the `spool_*` tools call `spool_find_sessions` and read a
returned session with `spool_read_messages`. Only a successful call verifies
agent access; Spool registers native tools, not an MCP server, so MCP resource
lists say nothing about it.

## Troubleshooting

| Symptom | Check |
| --- | --- |
| Tools are absent | `opencode plugin list` and the OpenCode log, such as for missing GitHub access. If only one agent lacks them, check its permissions. |
| `unauthorized` | The token file; run the probe above. |
| `reader_unavailable` | Run the probe from that machine and check the service. |
| Capture logs `source_alias_mismatch` or `source_alias_conflict`, then `disabled_until_reload` | Use the source's assigned alias, or an unused one for an unnamed source, and reload every instance. |
| `not_indexed` | Sessions lack complete projections; `/spool-repair` recaptures those still present locally. |

## Optional settings

- `repositoryRemote` selects the remote that identifies the repository; see
  [repository identity](retrieval.md#repository-identity).
- `serviceFile` points at a nondefault OpenCode service registration. For an
  unmanaged OpenCode server, set both `opencodeUrl` and `opencodePasswordFile`.
  Either way it must be the OpenCode process that loaded the plugin.
