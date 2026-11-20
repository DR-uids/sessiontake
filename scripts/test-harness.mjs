import assert from "node:assert/strict";
import { sourceThread } from "../src-tauri/resources/handoff.mjs";

const message = (kind, text) => ({
  type: "user/message",
  data: { source: { kind }, content: [{ type: "text", text }] },
});
const thread = sourceThread({
  session: { id: "fixture", cwd: "/source" },
  events: [
    message("runtime-context", "internal prompt"),
    message("user", "Goal"),
    {
      type: "assistant/message",
      data: {
        message: {
          content: [
            { type: "reasoning", text: "private" },
            { type: "text", text: "Progress" },
          ],
        },
      },
    },
    {
      type: "todo/write",
      data: {
        todos: [
          { status: "completed", content: "Finished" },
          { status: "pending", content: "Next" },
        ],
      },
    },
    message("user", "Latest request"),
    message("tool-jobs", "internal result"),
  ],
});
assert.deepEqual(thread.turns[0].items, [
  { type: "userMessage", content: [{ type: "text", text: "Goal" }] },
  { type: "agentMessage", text: "Progress" },
  { type: "plan", text: "Next" },
  { type: "userMessage", content: [{ type: "text", text: "Latest request" }] },
]);
console.log("Harness context: ok");
