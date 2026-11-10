import { Plugin } from "@opencode/plugin"
import { Collector } from "./collector"
import { configure } from "./config"
import { registerRetrieval } from "./retrieval"

// OpenCode re-evaluates local modules per Location; the registry must outlive imports.
const registry = Symbol.for("spool.collectors.v1")
const shared = globalThis as typeof globalThis & { [registry]?: Map<string, Collector> }
const collectors = shared[registry] ??= new Map<string, Collector>()

export default Plugin.define({
  id: "spool",
  async setup(ctx) {
    const config = await configure(ctx.options)
    const key = config.cachePrefix
    let collector = collectors.get(key)
    if (!collector || collector.closed) {
      collector = new Collector(config, ctx.app.version)
      collectors.set(key, collector)
    } else if (!collector.matches(config)) {
      throw new Error("Conflicting Spool configuration for the same source; reload all plugin instances after changing credentials or options")
    }
    await registerRetrieval(ctx, config)
    const active = collector
    await ctx.command.transform((editor) => editor.add({
      name: "spool-repair",
      description: "Re-export all local sessions to repair missed archive changes; runs in the background without a model call",
      execute: async () => { active.repair() },
    }))
    const remove = collector.attach(ctx)
    return async () => {
      if (await remove() && collectors.get(key) === collector) collectors.delete(key)
    }
  },
})
