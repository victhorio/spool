# Messages

The owner, from the reader, and agents, with `spool_send_message`, can send a
message to a session whose machine is online. It arrives in that session as a
prompt, as if typed there: it steers the agent if it is running, and starts it
if idle. Any source token may message any session, as it may read the whole
archive.

## Online and running

A machine is online while its plugin polls Spool, which it does continuously
whenever OpenCode runs with the plugin loaded. Each poll reports the sessions
running there, so `running` is at most about 25 seconds old. A session's status
is `running`, `idle` (online, not working, still messageable) or `offline` (no
poll in the last minute). Sending to an offline session fails with
`session_offline`; nothing is queued for later.

## Delivery

The plugin long-polls `/v1/poll`, which is also its heartbeat. Spool holds each
message until the machine collects it, and the plugin acknowledges it in the
next poll after prompting the session. A message OpenCode rejects, such as for a
deleted session, is dropped and logged without holding up the others. Messages
not collected within ten minutes are dropped, so a machine that comes back
later doesn't act on stale instructions. Sending confirms only that Spool
accepted the message.

A message from another agent starts with a `<spool>` line naming the sending
session and machine; the owner's text is prompted unchanged.
