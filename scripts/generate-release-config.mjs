import fs from "node:fs";
import path from "node:path";

const root = path.resolve(import.meta.dirname, "..");
const confPath = path.join(root, "app", "src-tauri", "tauri.conf.json");
const outPath = path.join(root, "app", "src-tauri", "tauri.release.conf.json");
const config = JSON.parse(fs.readFileSync(confPath, "utf8"));
const version = config.version;
const versions = { "app/src-tauri/tauri.conf.json": version };
const cargo = fs.readFileSync(path.join(root, "app", "src-tauri", "Cargo.toml"), "utf8");
versions["app/src-tauri/Cargo.toml"] = cargo.match(/^\[package\][^\[]{0,}?^version\s{0,}=\s{0,}"([^"]+)"/m)?.[1];
versions["app/package.json"] = JSON.parse(
  fs.readFileSync(path.join(root, "app", "package.json"), "utf8"),
).version;
if (Object.values(versions).some((v) => v !== version)) {
  const found = Object.entries(versions)
    .map(([file, v]) => file + "=" + (v ?? "(missing)"))
    .join(", ");
  throw new Error("Refusing release: versions differ: " + found);
}
const ref = process.env.GITHUB_REF_NAME || "";
if (!/^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/.test(version) || ref !== `v${version}`) {
  throw new Error(`Refusing release: tag ${ref || "(unset)"} != app version v${version}`);
}
const publicKey = (process.env.YUZUKI_UPDATER_PUBLIC_KEY || "").trim();
const privateKey = (process.env.TAURI_SIGNING_PRIVATE_KEY || "").trim();
if (!publicKey || !privateKey) {
  throw new Error(
    "Updater signing not configured: set repository variable YUZUKI_UPDATER_PUBLIC_KEY " +
    "and secret TAURI_SIGNING_PRIVATE_KEY (and secret TAURI_SIGNING_PRIVATE_KEY_PASSWORD if encrypted).",
  );
}
// Public key goes into a temporary, ignored build config. The private key
// stays in GitHub Actions secrets and is NEVER persisted to this file.
const release = {
  bundle: {
    createUpdaterArtifacts: true,
    icon: ["icons/32x32.png","icons/128x128.png","icons/128x128@2x.png","icons/icon.icns","icons/icon.ico"],
    // Built by scripts/build-sidecars.mjs for the matrix target before tauri build.
    externalBin: ["binaries/yuzuki", "binaries/yuzuki-mcp"],
  },
  plugins: {
    updater: {
      pubkey: publicKey,
      endpoints: ["https://github.com/avabbbb/Yuzuki/releases/latest/download/latest.json"],
      windows: { installMode: "passive" },
    },
  },
};
fs.writeFileSync(outPath, JSON.stringify(release, null, 2) + "\n", { mode: 0o600 });
console.log(`Prepared updater config for v${version}; no signing secrets written to disk.`);
