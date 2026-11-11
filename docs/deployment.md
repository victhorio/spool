# Deployment

Production is one Railway service, `spool`, with Railway PostgreSQL, served at
`https://spool.example.com` through Cloudflare. Railway builds
`server/Dockerfile` from GitHub `main` on every push. The service applies
migrations at startup, then serves on `PORT` (default 8080); Railway checks
`/healthz` at the origin, where it needs no credentials. It needs
`DATABASE_URL`, and `ACCESS_ISSUER` and `ACCESS_AUD` for the reader; without
those two it serves only the machine API.

Message polls wait in the server process, so it must run as a single instance.
Operator commands run the same binary with direct database access.

Spool keeps sessions after they are deleted locally, so it may hold their only
copy. Railway backs up the PostgreSQL volume daily, kept 6 days, and weekly,
kept 27 days. Purged content therefore stays in backups for up to 27 days.

## Cloudflare and Railway

- The `example.com` zone has a proxied `spool` CNAME to the target Railway shows
  for the custom domain, plus Railway's `_railway-verify.spool` TXT record.
  SSL/TLS mode is **Full**; Railway does not support Full (Strict) here.
- Bot Fight Mode stays off so machine uploads are not challenged. Session
  uploads (at most 64 MiB) fit Cloudflare's request limit.
- The service has no generated `*.up.railway.app` domain. Railway's edge still
  serves `spool.example.com` to anyone who connects to it directly, bypassing
  Cloudflare, which is why the server verifies Access assertions itself.

## Cloudflare Access

The Zero Trust team is `example.cloudflareaccess.com`, with GitHub as the login
method (an OAuth app whose callback is
`https://example.cloudflareaccess.com/cdn-cgi/access/callback`).

| Application | Destination | Policy |
| --- | --- | --- |
| Spool | `spool.example.com` | Allow the owner's email; GitHub only; 30-day session |
| Spool machine API | `spool.example.com/v1` | Bypass for everyone; Spool tokens protect it |

On the Railway service, `ACCESS_ISSUER` is `https://example.cloudflareaccess.com`
and `ACCESS_AUD` is the Spool application's AUD tag (Access → Applications →
Spool → Overview), not the machine API application's.

## Verify after deploying

- In a browser, `https://spool.example.com` asks for GitHub login, then shows the
  archive.
- `curl https://spool.example.com/v1/sync/sessions` returns 401 `unauthorized`
  (the bypass reaches Spool, which requires a token).
- Connecting to Railway directly returns 403 `forbidden`, because the origin
  rejects requests without an Access assertion:

  ```sh
  IP=$(dig +short <the custom domain's CNAME target> | tail -1)
  curl --resolve spool.example.com:443:$IP https://spool.example.com/api/sessions
  ```
- An agent's `spool_find_sessions` call succeeds from a configured machine.
