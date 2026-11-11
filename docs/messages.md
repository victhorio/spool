# Messages

The owner, from the reader, and agents, with a tool, can send a message to a
session whose machine is online. It arrives in that session as a prompt, as if
typed there: it steers the agent if it is running, and starts it if idle.

## Online and running

A machine is online while its plugin polls Spool, which it does continuously
whenever OpenCode is running with the plugin loaded. Each poll reports the
sessions running on that machine, so running reflects the start of the latest
poll, at most about 25 seconds ago. A session's `status` is therefore:

- `running`: its machine is online and the session is working.
- `idle`: its machine is online and the session is not working. It can still
  receive messages.
- `offline`: its machine has not polled in the last minute. Sending fails with
  409 `session_offline`; nothing is queued for later.

Sessions show their status in the reader, which marks running sessions in the
sidebar, and in `spool_find_sessions` results.

## Delivery

Spool stores each message until the session's machine collects it. The plugin
long-polls `POST /v1/poll` with its source token and the body
`{running: [session IDs], delivered: [message IDs]}`, at most 100 of each. The
server records the machine as online with those running sessions, deletes the
messages listed as delivered, and returns `{messages: [{id, session, text,
from}]}` as soon as there are any, or an empty list after 25 seconds. The plugin
prompts each session with `session.prompt`, then acknowledges the messages in its
next poll. A message OpenCode rejects, such as for a session or worktree that no
longer exists, is dropped and logged as `message_dropped` without holding up the
others; if OpenCode can't be reached, the whole poll is retried.
Messages not collected within ten minutes are dropped, so a machine that comes
back later does not act on stale instructions.

Waiting polls are woken in the server's own process, so the service runs as a
single instance. Deploy the server before updating plugins: against an older
server, a plugin's polls fail and it logs `delivery_failed`.

## Senders

`from` is null for the owner, whose text is prompted unchanged. For an agent it
is `{session, source}`: the sending session's native ID and its machine's alias
(or label), and the prompt starts with:

```text
<spool>This message was sent to you from an agent in ses_… in work-m5 through spool_send_message.</spool>
```

- The reader sends with `POST /api/messages` and `{session, text}`, behind
  Access and the same-origin checks of the rest of `/api/*`.
- Agents use the `spool_send_message` tool, `{session, text}`, which calls
  `POST /v1/messages` with `{session, text, from}`, `from` being the calling
  session. Any source token may message any session, like it may read the whole
  archive.

`session` is a [session reference](retrieval.md#session-references) to an
archived session; the tool also accepts a reader link. Text is 1 byte to 64 KiB,
not blank, without NUL. Both endpoints return `{id, status}` with the session's
status when sent; the tool returns `{sent: true, status}`. Errors: 400 `malformed_request`, 401 `unauthorized` (machines), 404
`not_found`, 409 `ambiguous_session` with `candidates`, 409 `session_offline`.

Sending confirms that Spool accepted the message, not that the agent read it.
Its effect appears in the session's archive once captured.
