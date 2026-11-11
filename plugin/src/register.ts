import type { Plugin } from "@opencode/plugin"
import type { Archive } from "./archive"
import { artifactTools } from "./artifacts"
import { messageTools } from "./messages"
import { retrievalTools } from "./retrieval"
import { bundledSkill } from "./skills"

/** Registers every spool tool under one namespace, with the skills that explain them. */
export async function registerTools(ctx: Plugin.Context, archive: Archive): Promise<void> {
  const skills = await Promise.all(["searching-sessions", "publishing-artifacts"].map(bundledSkill))
  await ctx.tool.transform((tools) => {
    tools.namespace({ name: "spool", description: "Search coding-agent sessions, message sessions on machines that are online, and publish documents for the user to read." })
    for (const tool of [...retrievalTools(archive), ...artifactTools(archive), ...messageTools(archive)]) {
      tools.add({
        name: tool.name,
        options: { namespace: "spool", codemode: false },
        description: tool.description,
        input: tool.input,
        execute: async (input, context) => ({ content: await tool.run(input, context) }),
      })
    }
  })
  await ctx.skill.transform((editor) => { for (const skill of skills) editor.add(skill) })
}
