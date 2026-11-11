import { expect, test } from "bun:test"
import { execFileSync } from "node:child_process"
import { mkdtemp, mkdir, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import path from "node:path"
import { normalizeRepository, resolveRepository } from "./repository"

test("repository URL normalization unifies protocols without erasing significant identity", () => {
  for (const value of ["https://GitHub.com/Owner/Repo.git/", "git@github.com:OWNER/Repo.GIT", "ssh://git@github.com:22/Owner/Repo/"]) {
    expect(normalizeRepository(value)).toBe("github.com/owner/repo")
  }
  expect(normalizeRepository("https://git.example/Owner/Repo.git")).toBe("git.example/Owner/Repo")
  expect(normalizeRepository("ssh://git@git.example:2222/Owner/Repo.git")).toBe("git.example:2222/Owner/Repo")
  expect(normalizeRepository("https://git.example:8443/Owner/Repo.git")).toBe("git.example:8443/Owner/Repo")
  expect(normalizeRepository("ssh://git@[::1]:2222/Repo.git")).toBe("[::1]:2222/Repo")
  for (const value of ["", "/tmp/Repo", "../Repo", "file:///tmp/Repo", "https://github.com/Owner/Repo?token=secret", "https://github.com/Owner/Repo\0", "https://github.com/Owner/Repo #bad"]) {
    expect(normalizeRepository(value)).toBeNull()
  }
})

test("real worktrees/clones and explicit remote selection use only fetch remotes", async () => {
  const root = await mkdtemp(path.join(tmpdir(), "spool-repositories-"))
  const repo = path.join(root, "repo; not a shell command")
  const git = (directory: string, ...args: string[]) => execFileSync("git", args, {
    cwd: directory, timeout: 5_000, maxBuffer: 64 * 1024, stdio: "pipe",
    env: { ...process.env, GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_SYSTEM: "/dev/null" },
  })
  try {
    await mkdir(repo)
    expect(await resolveRepository(repo)).toBeNull()
    git(repo, "init", "-q")
    git(repo, "-c", "user.name=Spool fixture", "-c", "user.email=fixture@example.invalid", "commit", "--allow-empty", "-qm", "fixture")
    expect(await resolveRepository(repo)).toBeNull()
    git(repo, "remote", "add", "other", "git@github.com:Owner/Repo.git")
    expect(await resolveRepository(repo)).toBe("github.com/owner/repo")
    git(repo, "remote", "add", "second", "https://elsewhere.example/Different/Repo.git")
    expect(await resolveRepository(repo)).toBeNull()
    expect(await resolveRepository(repo, "second")).toBe("elsewhere.example/Different/Repo")
    expect(await resolveRepository(repo, "not-present")).toBeNull()
    git(repo, "remote", "add", "origin", "https://github.com/OWNER/REPO.git")
    git(repo, "remote", "set-url", "--push", "origin", "https://wrong.example/Push/Only.git")
    expect(await resolveRepository(repo)).toBe("github.com/owner/repo")
    git(repo, "config", "url.https://github.com/.insteadOf", "gh:")
    git(repo, "remote", "set-url", "origin", "gh:Owner/Repo")
    expect(await resolveRepository(repo)).toBe("github.com/owner/repo")
    git(repo, "config", "remote.unset.fetch", "+refs/heads/*:refs/remotes/unset/*")
    expect(await resolveRepository(repo, "unset")).toBeNull()
    const worktree = path.join(root, "worktree")
    git(repo, "worktree", "add", "--detach", worktree)
    const clone = path.join(root, "clone")
    git(root, "clone", "--quiet", "--no-hardlinks", repo, clone)
    git(clone, "remote", "set-url", "origin", "ssh://git@github.com/Owner/Repo.git")
    for (const directory of [worktree, clone]) expect(await resolveRepository(directory)).toBe("github.com/owner/repo")
    git(repo, "remote", "set-url", "origin", "https://new.example/Actual/Change.git")
    expect(await resolveRepository(worktree)).toBe("new.example/Actual/Change")
    await rm(worktree, { recursive: true, force: true })
    expect(await resolveRepository(worktree)).toBeNull()
    expect(await resolveRepository("relative")).toBeNull()
  } finally { await rm(root, { recursive: true, force: true }) }
})
