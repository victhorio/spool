# Public deployment

Production is one Railway service, `spool`, built from `server/Dockerfile`, with
the Railway PostgreSQL service. It is served at `https://spool.example.com`
through Cloudflare. The service runs migrations at startup.

## Cloudflare and Railway

- The `example.com` zone has a proxied `spool` CNAME to the target Railway shows
  for the custom domain, plus Railway's `_railway-verify.spool` TXT record.
  SSL/TLS mode is **Full**; Railway does not support Full (Strict) here.
- Bot Fight Mode stays off so machine uploads are not challenged. Session
  uploads (at most 64 MiB) fit Cloudflare's request limit.
- Railway also serves a `*.up.railway.app` domain and the custom domain's CNAME
  target. Neither passes through Access, which is why the server verifies Access
  assertions itself. Remove the generated domain once no client uses it.

## Cloudflare Access

The Zero Trust team is `example.cloudflareaccess.com`, with GitHub as the login
method (an OAuth app whose callback is
`https://example.cloudflareaccess.com/cdn-cgi/access/callback`).

| Application | Destination | Policy |
| --- | --- | --- |
| Spool | `spool.example.com` | Allow the owner's email; GitHub only; 30-day session |
| Spool machine API | `spool.example.com/v1` | Bypass for everyone; Spool tokens protect it |

Set these variables on the Railway service:

- `ACCESS_ISSUER=https://example.cloudflareaccess.com`
- `ACCESS_AUD` = the Spool application's AUD tag (Access → Applications → Spool →
  Overview). The machine API application's tag is not used.

Without both variables, the service runs ingestion and the agent API only.

## Verify after deploying

- In a browser, `https://spool.example.com` asks for GitHub login, then shows the
  archive.
- `curl https://spool.example.com/v1/sync/sessions` returns 401 `unauthorized`
  (the bypass reaches Spool, which requires a token).
- `curl https://<railway-domain>/` returns 403 `forbidden`: the origin rejects
  requests without an Access assertion.
- An agent's `spool_find_sessions` call succeeds from a configured machine.
