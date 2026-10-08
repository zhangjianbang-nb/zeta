#!/usr/bin/env node
// zeta — npm 安装器：下载对应平台的预编译 nexus 二进制并透传参数。
const { execFileSync } = require("child_process");
const fs = require("fs");
const path = require("path");
const os = require("os");
const https = require("https");
const { URL } = require("url");

const VERSION = require("../package.json").version;
const REPO = "zhangjianbang-nb/zeta";

function platformTriple() {
  const a = process.arch, p = os.platform();
  if (p === "linux" && a === "x64") return "x86_64-unknown-linux-gnu";
  if (p === "linux" && a === "arm64") return "aarch64-unknown-linux-gnu";
  if (p === "darwin" && a === "x64") return "x86_64-apple-darwin";
  if (p === "darwin" && a === "arm64") return "aarch64-apple-darwin";
  throw new Error(`unsupported platform: ${p}-${a}`);
}

function download(url, dest) {
  return new Promise((resolve, reject) => {
    const get = (u, redirects) => {
      https.get(u, (res) => {
        if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location && redirects < 5) {
          return get(new URL(res.headers.location, u).href, redirects + 1);
        }
        if (res.statusCode !== 200) return reject(new Error(`HTTP ${res.statusCode} for ${u}`));
        const f = fs.createWriteStream(dest);
        res.pipe(f);
        f.on("finish", () => f.close(resolve));
        f.on("error", reject);
      }).on("error", reject);
    };
    get(url, 0);
  });
}

async function ensureBinary() {
  const dir = path.join(__dirname, "..", "bin");
  const ext = os.platform() === "win32" ? ".exe" : "";
  const bin = path.join(dir, `zeta-${platformTriple()}${ext}`);
  if (fs.existsSync(bin)) return bin;
  const url = `https://github.com/${REPO}/releases/download/v${VERSION}/zeta-${platformTriple()}.tar.gz`;
  const archive = bin + ".tar.gz";
  console.error(`[zeta] downloading ${url}`);
  await download(url, archive);
  execFileSync("tar", ["-xzf", archive, "-C", dir]);
  fs.unlinkSync(archive);
  fs.chmodSync(bin, 0o755);
  return bin;
}

async function main() {
  try {
    const bin = await ensureBinary();
    const r = execFileSync(bin, process.argv.slice(2), { stdio: "inherit" });
    process.exitCode = r === 0 ? 0 : 0;
  } catch (e) {
    console.error("[zeta] failed:", e.message);
    console.error("fallback: install Rust and run `cargo install --git https://github.com/" + REPO + "`");
    process.exit(1);
  }
}

main();
