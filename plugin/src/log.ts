import { ClientError } from "@opencode/client"

/** One structured log line. Fields must never hold session content or credentials. */
export function log(event: string, fields: Record<string, unknown> = {}): void {
  console.info(JSON.stringify({ component: "spool", event, ...fields }))
}

/** Carries a code safe to log; `retryable` failures are tried again later. */
export class SyncError extends Error {
  constructor(readonly code: string, readonly retryable = true, readonly revision?: string | null) { super(code) }
}

/** A loggable code for any failure, which never includes its message. */
export function errorCode(error: unknown): string {
  if (error instanceof SyncError) return error.code
  if (error instanceof ClientError) {
    switch (error.reason) {
      case "Transport": case "UnexpectedStatus": case "UnsupportedContentType": case "MalformedResponse": case "SseEventTooLarge":
        return `sdk_${error.reason}`
    }
  }
  if (error && typeof error === "object" && "_tag" in error) {
    switch (error._tag) {
      case "UnauthorizedError": case "InvalidRequestError": case "ServiceUnavailableError": case "LocationNotFoundError": case "InvalidCursorError": case "UnknownError":
        return `sdk_${error._tag}`
    }
  }
  if (error instanceof Error) {
    if (error.name === "AbortError") return "aborted"
    if (error.name === "TimeoutError") return "timeout"
    if (error instanceof TypeError) return "type_error"
  }
  return "unexpected_error"
}
