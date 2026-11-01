// Builds the yuzuki CLI and the yuzuki-mcp server for one Rust target and
// copies them to app/src-tauri/binaries/<name>-<target triple>[.exe], the
// layout Tauri's bundle.externalBin expects. The installers then ship both
// next to the Desktop executable, where yuzuki --launch looks for it.
//
// Usage: node scripts/build-sidecars.mjs [--target <triple>] [--debug]
// Default target: the host triple reported by rustc -vV.
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";

const root = path.resolve(import.meta.dirname, "..");
const args = process.argv.slice(2);
const debug = args.includes("--debug");
const at = args.indexOf("--target");
let target = at >= 0 ? args[at + 1] : undefined;
if (at >= 0 && !target) throw new Error("--target needs a triple");
if (!target) {
  const info = execFileSync("rustc", ["-vV"], { encoding: "utf8" });
  target = info.match(/^host: (\S+)$/m)?.[1];
  if (!target) throw new Error("cannot read the host triple from rustc -vV");
}

// [cargo package, binary name] pairs; the binary name is what externalBin lists.
const sidecars = [
  ["yuzuki-cli", "yuzuki"],
  ["yuzuki-mcp", "yuzuki-mcp"],
];
const cargoArgs = ["build", "--locked", "--target", target];
if (!debug) cargoArgs.push("--release");
for (const [pkg] of sidecars) cargoArgs.push("-p", pkg);
execFileSync("cargo", cargoArgs, { cwd: root, stdio: "inherit" });

const exe = target.includes("windows") ? ".exe" : "";
const built = path.join(root, "target", target, debug ? "debug" : "release");
const outDir = path.join(root, "app", "src-tauri", "binaries");
fs.mkdirSync(outDir, { recursive: true });
for (const [, bin] of sidecars) {
  const from = path.join(built, bin + exe);
  const to = path.join(outDir, bin + "-" + target + exe);
  if (!fs.existsSync(from)) throw new Error("cargo did not produce " + from);
  fs.copyFileSync(from, to);
  if (!exe) fs.chmodSync(to, 0o755);
  console.log("sidecar " + path.relative(root, to) + " (" + fs.statSync(to).size + " bytes)");
}
