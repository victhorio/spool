/** A spool tool; its result is JSON text for the model. */
export interface Tool {
  name: string
  description: string
  input: Record<string, unknown>
  run(input: unknown, context: { sessionID: string; signal: AbortSignal }): Promise<string>
}

/** A failed tool call's result: a code the model can act on, and any detail. */
export function toolError(code: string, detail: Record<string, unknown> = {}): string {
  return JSON.stringify({ error: { code, ...detail } })
}
