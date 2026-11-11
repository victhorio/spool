import type { Config } from "./config"

export type Archive = Pick<Config, "url" | "token">

/** A request to the archive with this machine's token. It never follows redirects, which could send the token elsewhere. */
export function archiveFetch(archive: Archive, path: string, init: RequestInit & { timeout: number }): Promise<Response> {
  const { timeout, signal, headers, ...rest } = init
  return fetch(`${archive.url}${path}`, {
    ...rest, redirect: "error", credentials: "omit",
    headers: { authorization: `Bearer ${archive.token}`, ...headers },
    signal: AbortSignal.any([AbortSignal.timeout(timeout), ...(signal ? [signal] : [])]),
  })
}

/** The UTF-8 body, or undefined once it exceeds `limit` bytes. */
export async function boundedText(response: Response, limit: number): Promise<string | undefined> {
  if (!response.body) return ""
  const reader = response.body.getReader()
  const chunks: Uint8Array[] = []
  let length = 0
  try {
    for (;;) {
      const { done, value } = await reader.read()
      if (done) break
      length += value.byteLength
      if (length > limit) return undefined
      chunks.push(value)
    }
  } finally { await reader.cancel().catch(() => {}) }
  return new TextDecoder("utf-8", { fatal: true }).decode(Buffer.concat(chunks))
}
