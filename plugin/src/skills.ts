import { readFile } from "node:fs/promises"
import { fileURLToPath } from "node:url"
import { Skill } from "@opencode/plugin"

/** Loads a skill bundled in ../skills/<name>/SKILL.md as spool.<name>. */
export async function bundledSkill(name: string) {
  const location = new URL(`../skills/${name}/SKILL.md`, import.meta.url)
  const source = await readFile(location, "utf8")
  const frontmatter = /^---\nname: ([^\n]+)\ndescription: ([^\n]+)\n---\n([\s\S]*)$/.exec(source)
  if (!frontmatter) throw new Error(`Invalid bundled ${name} skill`)
  return {
    id: Skill.ID.make(`spool.${name}`), name: Skill.Name.make(frontmatter[1]!),
    description: frontmatter[2]!, path: fileURLToPath(location) as Skill.Info["path"],
    content: frontmatter[3]!.trim(),
  }
}
