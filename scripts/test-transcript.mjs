import assert from "node:assert/strict";
import { build } from "esbuild";

const { outputFiles } = await build({
  entryPoints: ["src/transcript.ts"],
  bundle: true,
  write: false,
  platform: "node",
  format: "esm",
});
const moduleUrl = "data:text/javascript;base64," + Buffer.from(outputFiles[0].contents).toString("base64");
const { exportFilename } = await import(moduleUrl);

const date = new Date(2026, 10, 19);
assert.equal(exportFilename('原/文: 标题?', "jsonl", date), "2026-11-19-原-文- 标题-.jsonl");
assert.ok(Buffer.byteLength(exportFilename("标题".repeat(500), "md", date)) < 255);
console.log("Transcript export: ok");
