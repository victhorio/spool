// A synthetic Cloudflare Access team: a certificate endpoint and a signed assertion
// for the local server. The key exists only in memory for one validation run.

const encode = (value: unknown) => Buffer.from(JSON.stringify(value)).toString("base64url")

export async function startAccess() {
  const audience = "syntheticaudience"
  const kid = "synthetic-validation"
  const { publicKey, privateKey } = await crypto.subtle.generateKey(
    { name: "RSASSA-PKCS1-v1_5", modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: "SHA-256" },
    true,
    ["sign", "verify"],
  )
  const { n, e } = await crypto.subtle.exportKey("jwk", publicKey)
  const server = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    fetch: (request) => new URL(request.url).pathname === "/cdn-cgi/access/certs"
      ? Response.json({ keys: [{ kid, kty: "RSA", alg: "RS256", use: "sig", n, e }] })
      : new Response(null, { status: 404 }),
  })
  const issuer = server.url.origin
  const now = Math.floor(Date.now() / 1000)
  const signed = `${encode({ alg: "RS256", kid, typ: "JWT" })}.${encode({
    iss: issuer, aud: [audience], iat: now, nbf: now, exp: now + 3600, email: "owner@example.invalid", type: "app",
  })}`
  const signature = await crypto.subtle.sign("RSASSA-PKCS1-v1_5", privateKey, new TextEncoder().encode(signed))
  return {
    env: { ACCESS_ISSUER: issuer, ACCESS_AUD: audience },
    assertion: `${signed}.${Buffer.from(signature).toString("base64url")}`,
    stop: () => server.stop(true),
  }
}
