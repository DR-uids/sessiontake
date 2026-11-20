// Loaded for one dsh invocation. The desktop and CLI share sessionPersistence.
import { isDeepStrictEqual } from "node:util";
import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
export const name = "memtrans-handoff";
export const inject = [
  "sessionPersistence",
  "sessionQuery",
  "workspaceRegistry",
  "appExit",
];

// The desktop sidebar groups Sessions from Workspace account membership, not
// from their cwd. `sessionPersistence.create` alone therefore leaves a Session
// in the ungrouped bucket even though its directory belongs to a project.
async function workspaceAt(ctx, cwd) {
  const registry = ctx.get("workspaceRegistry");
  if (registry === undefined) throw new Error("harness_workspace_missing");
  let workspace;
  try {
    workspace = await registry.resolveByPath(cwd);
  } catch {
    throw new Error("harness_workspace_missing");
  }
  if (workspace === undefined) throw new Error("harness_workspace_missing");
  return workspace;
}

async function attachWorkspace(workspace, id) {
  try {
    await workspace.attachSession(id);
  } catch {
    throw new Error("harness_workspace_attach");
  }
  if (!workspace.sessionIds.includes(id))
    throw new Error("harness_workspace_attach");
}

function handoffEvents({ id, title, body }) {
  return [
    {
      type: "session/title",
      data: { title, messageSeqs: [], source: { kind: "user" } },
    },
    // Harness marks a session as blank until it sees a turn/start event.
    { type: "turn/start", data: { turn: 1 } },
    {
      type: "user/message",
      surfaceOp: "append",
      data: {
        role: "user",
        id,
        source: { kind: "user" },
        content: [{ type: "text", text: body }],
      },
    },
    { type: "turn/end", data: { turn: 1, reason: { kind: "completed" } } },
  ];
}

function matchesPrefix(events, wanted) {
  return events
    .slice(0, wanted.length)
    .every(
      (event, i) =>
        event.seq === i &&
        event.type === wanted[i].type &&
        event.surfaceOp === wanted[i].surfaceOp &&
        isDeepStrictEqual(event.data, wanted[i].data),
    );
}

async function transfer(ctx, request) {
  const { id, cwd } = request;
  const store = ctx.get("sessionPersistence");
  const query = ctx.get("sessionQuery");
  const wanted = handoffEvents(request);
  const delivered = (events) =>
    events.length >= wanted.length && matchesPrefix(events, wanted);
  let handle;
  try {
    const existing = await query.readSession(id);
    if (delivered(existing.events)) return;
    handle = await store.open(id, "write");
  } catch (error) {
    if (error.code !== "SESSION_QUERY_SESSION_NOT_FOUND") throw error;
    handle = await store.create({
      version: 4,
      id,
      createdAt: Date.now(),
      cwd,
      isSeeded: false,
    });
  }
  try {
    const { events } = await handle.read();
    if (!delivered(events)) {
      // Resume only our own immutable prefix after an interrupted write.
      if (!matchesPrefix(events, wanted))
        throw new Error("Existing session contains a different handoff");
      const time = Date.now();
      const missing = wanted
        .map((event, seq) => ({ ...event, seq, time }))
        .slice(events.length);
      await handle.append(missing);
      await handle.flush();
    }
  } finally {
    await handle.close();
  }
  if (!delivered((await query.readSession(id)).events))
    throw new Error("Message read-back failed");
}

async function listSessions(ctx) {
  const registry = join(
    process.env.DSH_HOME || join(homedir(), ".dsh"),
    "storages",
    "workspace.json",
  );
  const archived = new Set(
    existsSync(registry)
      ? JSON.parse(readFileSync(registry, "utf8")).global?.archivedSessionIds
      : [],
  );
  const cache = ctx.get("sessionProjectionCache");
  const sessions = [];
  for (const { header } of await ctx.get("sessionQuery").listSessions()) {
    if (header.origin === "subagent" || archived.has(header.id)) continue;
    const values = cache?.cachedSnapshot(header, [
      "title",
      "sessionListMetadata",
    ])?.values;
    if (values?.sessionListMetadata?.blank) continue;
    let title = values?.title;
    let updatedAt = Math.max(
      header.createdAt,
      values?.sessionListMetadata?.lastPromptAt || 0,
    );
    if (!title) {
      try {
        const { events } = await ctx.get("sessionQuery").readSession(header.id);
        title = events.findLast((event) => event.type === "session/title")?.data
          .title;
        updatedAt = events.at(-1)?.time ?? updatedAt;
      } catch {
        // Keep sessions with an unreadable log selectable so read can report the error.
      }
    }
    sessions.push({
      id: header.id,
      title: title || header.id,
      project: header.cwd || null,
      updatedAt: Math.floor(updatedAt / 1000),
    });
  }
  return sessions;
}

function visibleText(content) {
  return (Array.isArray(content) ? content : [])
    .filter((part) => part.type === "text" && typeof part.text === "string")
    .map((part) => part.text)
    .join("\n");
}

export function sourceThread({ session: header, events }) {
  let name, first, latest, progress, plan;
  for (const { type, data } of events) {
    if (type === "session/title") name = data.title;
    if (type === "user/message" && data.source?.kind === "user") {
      const text = visibleText(data.content);
      if (text.trim()) {
        if (first === undefined) first = text;
        else latest = text;
      }
    }
    if (type === "assistant/message") {
      const text = visibleText(data.message?.content);
      if (text.trim()) progress = text;
    }
    if (type === "todo/write") {
      plan = data.todos
        .filter((todo) => todo.status !== "completed")
        .map((todo) => todo.content)
        .join("\n");
    }
  }
  const userItem = (text) => ({
    type: "userMessage",
    content: [{ type: "text", text }],
  });
  const items = [];
  if (first !== undefined) items.push(userItem(first));
  if (progress) items.push({ type: "agentMessage", text: progress });
  if (plan) items.push({ type: "plan", text: plan });
  if (latest !== undefined) items.push(userItem(latest));
  return {
    id: header.id,
    cwd: header.cwd,
    name,
    updatedAt: events.at(-1)?.seq,
    turns: [{ items }],
  };
}

async function dispatch(ctx, request) {
  if (request.action === "list") return { sessions: await listSessions(ctx) };
  if (request.action === "read") {
    return {
      thread: sourceThread(
        await ctx.get("sessionQuery").readSession(request.id),
      ),
    };
  }
  // Validate the target Workspace before writing, so a missing project cannot
  // leave another ungrouped Session behind.
  const workspace = await workspaceAt(ctx, request.cwd);
  if (request.action === "repair") {
    // Re-register Sessions delivered by older builds that only wrote the log.
    const ids = Array.isArray(request.ids) ? request.ids : [request.id];
    for (const id of ids) await attachWorkspace(workspace, id);
    return { repaired: ids.length, workspace: String(workspace.id) };
  }
  await transfer(ctx, request);
  await attachWorkspace(workspace, request.id);
  return { delivered: request.id, workspace: String(workspace.id) };
}

async function readRequest() {
  process.stdin.setEncoding("utf8");
  let input = "";
  for await (const chunk of process.stdin) input += chunk;
  return JSON.parse(input);
}

export function apply(ctx) {
  const exit = ctx.get("appExit");
  readRequest()
    .then((request) => dispatch(ctx, request))
    .then((reply) => {
      process.stdout.write(JSON.stringify(reply) + "\n", () => exit(0));
    })
    .catch((error) => {
      process.stdout.write(
        JSON.stringify({ error: String(error.message).slice(0, 400) }) + "\n",
        () => exit(1),
      );
    });
}
