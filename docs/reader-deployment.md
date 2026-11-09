# Private production reader

Deploy a second Railway service in the existing Spool project. It reads the same
live PostgreSQL database as ingestion; there is no archive copy or extra sync.
New uploads are visible when the browser refreshes. The existing public ingestion
service and its credentials remain unchanged.

The reader has no application login. Every permitted tailnet client can read
every archived source. It is not suitable for public hosting or per-source access.
Deployment, database provisioning and tailnet changes are explicit operator
actions, not consequences of building the image.

## Trusted tailnet

This deployment uses a single-user tailnet containing only the owner's mutually
trusted machines. Keep its existing allow-all grant and SSH rules unchanged.
Enroll the reader as another device owned by that user; no tags, tagOwners
declarations, per-device allowlists or port restrictions are required. New devices
continue to receive the existing unrestricted intra-tailnet access automatically.

Membership in this tailnet is the reader's access boundary: every permitted peer
can read every archived source. Tailscale userspace networking also forwards
otherwise-unhandled incoming TCP ports to the same port on localhost, so the
health listener is reachable too. This is accepted within the trusted tailnet;
the Serve JSON is not a port firewall. Do not enable Funnel, subnet routes,
exit-node routing, SSH or additional proxies on the reader. If the tailnet's trust
model changes, revisit access policy before admitting untrusted devices or users.
Removing Serve configuration alone does not disable access; stop the service or
revoke its tailnet access instead.

## Provision a database login

After ingestion migrations have run, use the archive owner with CREATEROLE to run
`deploy/reader-role.sql` against the existing database. It atomically creates
`spool_reader` as NOLOGIN, with column-level SELECT grants for the reader and no
credential/hash access. An existing role is an error, not silently repurposed.
It does not change PUBLIC privileges: audit PUBLIC grants, role memberships and
security-definer functions first. PUBLIC must not grant archive writes, sensitive
reads or CREATE on the public schema.

```sh
psql "$ADMIN_DATABASE_URL" -X -v ON_ERROR_STOP=1 -f deploy/reader-role.sql
psql "$ADMIN_DATABASE_URL" -X
```

In that interactive psql session, assign a generated private password with
`\password spool_reader`, then run `ALTER ROLE spool_reader LOGIN;`. The password
prompt avoids plaintext in SQL history. Store the password in your secret manager
and Railway, not this repository or a conversation. PostgreSQL's read-only default
is an extra accident barrier; the grants still deny writes if that default is
disabled. Revisit the explicit grants when the reader begins using new columns;
do not grant all future tables automatically.

Construct the reader's `DATABASE_URL` with this login and Railway's **private**
database hostname. URI-encode the password. Do not reference the ingestion owner's
DATABASE_URL or give the reader migration/admin credentials.

## Railway service

Create a separate service named `spool-reader` from this repository, in the same
project, environment and region as PostgreSQL. Configure:

| Setting | Value |
| --- | --- |
| Root directory | Repository root |
| Dockerfile | `deploy/reader.Dockerfile` via `RAILWAY_DOCKERFILE_PATH` |
| Start/pre-deploy commands | Leave unset; do not run migrations |
| Replicas | One; never share the identity volume between replicas |
| Serverless/sleeping | Disabled; this is an always-on tailnet node |
| Persistent volume | Dedicated volume at `/var/lib/tailscale`; identity only, not the archive |
| Healthcheck | `/healthz`, with `PORT=9002`, timeout 300 seconds |
| Shutdown grace | At least 15 seconds |
| Restart policy | On failure |
| Memory | Start with 500 MB; measure actual use |
| Public HTTP domain/TCP proxy | None |

Set these service variables privately:

- `DATABASE_URL`: the restricted login above.
- `TS_AUTHKEY`: a one-off, non-ephemeral, untagged enrollment key under the
  owner's account; preapprove it if device approval is enabled. A one-day key
  expiry is sufficient for initial enrollment and does not expire the device.
- `TS_HOSTNAME=spool-archive`: a device label, not an authentication check.
- `PORT=9002` and `RAILWAY_DOCKERFILE_PATH=deploy/reader.Dockerfile`.
- `TS_DEBUG_MTU=1200`: the production Railway interface has MTU 1316, too small
  for Tailscale's default 1280-byte packets plus tunnel overhead. Without this,
  small responses succeeded but the JavaScript bundle stalled; with it, the
  complete bundle downloaded and matched the container's SHA-256. This is a
  version-specific Tailscale control, not a stable configuration API. Use the
  reader's IPv4 URL: IPv6 did not connect with this below-minimum IPv6 MTU.
  Recheck full asset transfers and connectivity after Tailscale/platform updates.

The image initializes the root-owned volume, then runs Rust, containerboot and
tailscaled as UID 10001. PID 1's minimal init remains root; Docker health commands
drop to UID 10001 before checking. Do not force a different runtime UID without
preparing writable state/runtime directories. No privileged container, TUN mount,
NET_ADMIN, host networking or published archive port is required. Leave other
`TS_*` overrides unset; the local validation harness's control-server overrides
are not production settings.

Preserve the whole state volume and use it only for this service. Once enrollment
and a replacement-container restart without the key have been verified, remove
`TS_AUTHKEY`. Expiring the enrollment key does not revoke an enrolled node;
revocation/node-key expiry are separate. Untagged devices have node-key expiry
enabled by default; for unattended operation, disable expiry for this new server
in the Tailscale Machines page. Losing the volume requires reenrollment and may
change the IP. Never copy production identity state into a preview.
Railway volumes prevent overlapping deployments, so expect brief reader downtime
on redeploy. Ingestion is independent and remains available.

## Readiness and first connection

Startup checks Rust's private `/healthz`, which performs a bounded database read,
before starting Tailscale. Either supervised process exiting stops its sibling
and fails the container, including containerboot's zero exit on daemon death.
SIGTERM shuts down both. The fixed TCP mapping forwards tailnet 8090 directly to
Rust's loopback 8090 without rewriting HTTP headers.

Railway's health port exposes only Tailscale's `/healthz`, not archive routes. It
reports an assigned tailnet IP, **not complete archive readiness**, and Railway's
check runs at deployment, not continuously. The image's exec check `reader-health`
also requires a readable database, a Running node and the exact intended Serve
configuration; run it during deployment validation. Do not assume Railway runs
Docker's periodic HEALTHCHECK or restarts an unhealthy-but-running process.

Run inside the reader container:

```sh
gosu 10001:10001 reader-health
tailscale --socket=/run/spool/tailscaled.sock ip -4
```

Open `http://<that-literal-tailscale-IP>:8090` from an authorized device. Tailscale
encrypts the HTTP data path. MagicDNS/custom hostnames, HTTPS origins and HTTP
reverse proxies are intentionally unsupported by the reader's Host/Origin guards.
Do not generate a public Railway domain for either 8090 or the health listener.

Before calling deployment complete: verify reads from an allowed tailnet device,
no public archive ingress, live upload visibility, database write/hash-read
denials, and persistent identity after replacement without the enrollment key.
Confirm the existing tailnet policy is unchanged. Provisioning and deployment
remain unperformed until explicitly authorized.

## Local validation

`bun run reader:validate` builds the real production image and a small test-only
control plane using the pinned Tailscale release's own integration-test server.
It runs actual userspace clients and encrypted TCP forwarding against synthetic
Spool data and PostgreSQL, not a substitute HTTP proxy. The test network has no
external egress, its DERP map names only a local relay, and enrollment keys work
only in that disposable control plane. Your installed Tailscale, real
conversations, personal credentials and models are not used.

Checks cover built assets/transcripts, restricted credentials, private binding,
Host/Origin/write rejection, port policy, live updates, missing/invalid enrollment,
root-owned state volume initialization, non-root services, restart without the
key, persistent node ID/IP, signals and daemon failure. Containers, networks and
volumes are removed; failed runs retain logs under `.local/reader-validate-*`.
Interruptions wait for an in-flight Docker command before cleaning up.
The fixture's TCP8090-only policy exercises optional isolation, not a requirement
to tighten this deployment's trusted-tailnet policy.
This does not prove your production tailnet policy, Railway networking/volume
behavior or a production deployment. Those require the separately authorized
go-live checks above.
