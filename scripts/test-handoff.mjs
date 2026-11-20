// End-to-end check of the DSH handoff plugin in an isolated DSH_HOME: the
// delivered Session must be written AND registered in the selected Workspace.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import {
  cpSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  realpathSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";

const cli =
  process.env.DSH_CLI ??
  "/Applications/DeepSeek Harness.app/Contents/Resources/runtime/cli/bin/dsh";
const profile = join(homedir(), ".dsh", "profiles", "headless");
const patch =
  process.env.DSH_PATCH ??
  join(import.meta.dirname, "..", "src-tauri", "resources", "handoff.yml");
if (!existsSync(cli) || !existsSync(profile)) {
  console.log("Harness handoff: skipped (no installed DSH headless profile)");
  process.exit(0);
}

const root = mkdtempSync(join(tmpdir(), "memtrans-handoff-"));
const home = join(root, "home");
const project = join(root, "Memtrans");
const other = join(root, "Other");
const session = "session-fixture-handoff";
const workspaceId = "ws-fixture";

function call(request) {
  let output;
  try {
    output = execFileSync(cli, ["headless", "--patch", patch, "-"], {
      cwd: join(import.meta.dirname, ".."),
      env: { ...process.env, DSH_HOME: home },
      input: JSON.stringify(request),
      encoding: "utf8",
      timeout: 60_000,
    });
  } catch (error) {
    // The plugin reports request failures as JSON and exits non-zero.
    output = error.stdout;
    if (!output) throw error;
  }
  return JSON.parse(output.trim().split("\n").at(-1));
}

function registry() {
  return JSON.parse(
    readFileSync(join(home, "storages", "workspace.json"), "utf8"),
  );
}

try {
  mkdirSync(project, { recursive: true });
  mkdirSync(other, { recursive: true });
  mkdirSync(join(home, "profiles"), { recursive: true });
  cpSync(profile, join(home, "profiles", "headless"), { recursive: true });
  mkdirSync(join(home, "storages"), { recursive: true });
  const now = new Date().toISOString();
  writeFileSync(
    join(home, "storages", "workspace.json"),
    JSON.stringify({
      unit: { name: "workspace", version: 2 },
      global: {
        initialized: true,
        workspaceIds: [workspaceId],
        archivedSessionIds: [],
        pinnedSessionIds: [],
      },
      tables: {
        workspaces: {
          [workspaceId]: {
            path: realpathSync(project),
            title: "Memtrans",
            sessionIds: [],
            createdAt: now,
            updatedAt: now,
          },
        },
      },
    }),
  );

  const request = {
    id: session,
    title: "Fixture handoff",
    body: "[Memtrans session-fixture-handoff]\n\nhello",
    cwd: project,
  };
  const first = call(request);
  assert.equal(first.delivered, session);
  assert.equal(first.workspace, workspaceId);
  assert.ok(registry().tables.workspaces[workspaceId].sessionIds.includes(session));

  // Re-delivery must reuse the same log and remain a single Workspace member.
  const second = call(request);
  assert.equal(second.delivered, session);
  assert.deepEqual(
    registry().tables.workspaces[workspaceId].sessionIds,
    [session],
  );
  const listed = call({ action: "list" }).sessions;
  assert.equal(listed.filter((row) => row.id === session).length, 1);
  assert.equal(listed.find((row) => row.id === session).project, project);

  // Repair re-registers a Session that an older build left in the ungrouped
  // bucket, without touching its log or duplicating the message.
  const file = join(home, "storages", "workspace.json");
  const broken = registry();
  broken.tables.workspaces[workspaceId].sessionIds = [];
  writeFileSync(file, JSON.stringify(broken));
  const repaired = call({ action: "repair", cwd: project, ids: [session] });
  assert.equal(repaired.repaired, 1);
  assert.deepEqual(
    registry().tables.workspaces[workspaceId].sessionIds,
    [session],
  );

  // A directory without a Workspace must fail before a Session is written.
  const missing = call({ ...request, id: `${session}-other`, cwd: other });
  assert.equal(missing.error, "harness_workspace_missing");
  assert.ok(!existsSync(join(home, "sessions", `--${other.slice(1).replaceAll("/", "-")}--`)));
  console.log("Harness handoff: ok");
} finally {
  rmSync(root, { recursive: true, force: true });
}
