import * as esbuild from "esbuild";
import { cp, mkdir } from "node:fs/promises";

const dev = process.argv.includes("--dev");
await mkdir("dist", { recursive: true });
await cp("index.html", "dist/index.html");
await cp("src/logo.png", "dist/logo.png");
const config = {
  entryPoints: ["src/main.ts", "src/style.css"],
  outdir: "dist",
  bundle: true,
  format: "esm",
  target: "safari15",
  minify: !dev,
};
if (dev) {
  const ctx = await esbuild.context(config);
  await ctx.watch();
  await ctx.serve({ servedir: "dist", host: "127.0.0.1", port: 1420 });
} else {
  await esbuild.build(config);
}
