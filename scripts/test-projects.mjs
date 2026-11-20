import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { build } from "esbuild";

const dir = await mkdtemp(join(tmpdir(), "memtrans-projects-"));
try {
  const outfile = join(dir, "projects.mjs");
  await build({
    entryPoints: ["src/projects.ts"],
    outfile,
    bundle: true,
    format: "esm",
    platform: "node",
  });
  const { groupProjects, baseName, codexWorkspaceTag } = await import(pathToFileURL(outfile).href);
  assert.equal(codexWorkspaceTag("/Users/u/Documents/ChatGPT/Memtrans"), "ChatGPT");
  assert.equal(codexWorkspaceTag("/Users/u/Documents/chatgpt"), "ChatGPT");
  assert.equal(codexWorkspaceTag("/Users/u/Code/Memtrans"), "Codex");
  assert.equal(codexWorkspaceTag("/Users/u/Code/ChatGPT-client"), "Codex");
  assert.equal(codexWorkspaceTag(null), "Codex");

  const rows = [
    {
      project: "/tmp/vibe-kanban/worktrees/2222-header/sample-report",
      projectExists: true,
      worktree: "2222-header",
    },
    {
      project: "/Users/u/Code/.vibe-kanban-workspaces/1111-/sample-report",
      projectExists: true,
      worktree: "1111-",
    },
    {
      project: "/Users/u/Code/examples/sample-report",
      projectExists: true,
      worktree: null,
    },
    {
      project: "/tmp/vibe-kanban/worktrees/dead-/sample-report",
      projectExists: false,
      worktree: "dead-",
    },
  ];
  const index = groupProjects(rows);
  const repo = index.groups.find((group) =>
    group.paths.includes("/Users/u/Code/examples/sample-report"),
  );
  assert.ok(repo, "main checkout and worktrees form one project");
  assert.equal(repo.name, "sample-report");
  assert.equal(repo.paths.length, 4, "missing worktrees remain part of the repository");
  assert.deepEqual(repo.worktrees, ["1111-", "2222-header", "dead-"]);
  assert.equal(
    index.pathLabels.get(
      "/tmp/vibe-kanban/worktrees/2222-header/sample-report",
    ),
    "sample-report",
  );

  const plain = groupProjects([
    { project: "/Users/u/Code/Memtrans", projectExists: true, worktree: null },
    {
      project: "/Users/u/Documents/ChatGPT/Memtrans",
      projectExists: true,
      worktree: null,
    },
    { project: "/Users/u/Code/Missing", projectExists: false, worktree: null },
  ]);
  assert.equal(plain.groups.length, 3, "ordinary same-name paths stay separate");
  assert.deepEqual(
    plain.groups.map((group) => group.name).sort(),
    ["Memtrans · ChatGPT", "Memtrans · Code", "Missing"],
  );
  assert.equal(plain.pathLabels.get("/Users/u/Code/Missing"), "Missing");

  const worktreeAndPlain = groupProjects([
    {
      project: "/tmp/vibe-kanban/worktrees/1111-x/示例笔记",
      projectExists: true,
      worktree: "1111-x",
    },
    {
      project: "/Users/u/Code/示例笔记",
      projectExists: true,
      worktree: null,
    },
    {
      project: "/Users/u/Code/other/示例笔记",
      projectExists: true,
      worktree: null,
    },
  ]);
  assert.equal(
    worktreeAndPlain.groups.filter((group) => group.name.startsWith("示例"))
      .length,
    3,
    "an ambiguous main checkout is not folded into the worktree group",
  );

  assert.equal(baseName("/a/b/"), "b");
  assert.deepEqual(index.groups[0].paths, index.groups[0].paths.slice().sort());
  console.log("projects: ok");
} finally {
  await rm(dir, { recursive: true, force: true });
}
