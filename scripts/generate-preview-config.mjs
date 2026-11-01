import fs from "node:fs";
const icons = ["icons/32x32.png","icons/128x128.png","icons/128x128@2x.png","icons/icon.icns","icons/icon.ico"];
fs.writeFileSync(
  "app/src-tauri/tauri.preview.conf.json",
  JSON.stringify({ bundle: { icon: icons, externalBin: ["binaries/yuzuki", "binaries/yuzuki-mcp"] } }, null, 2) + "\n",
);
