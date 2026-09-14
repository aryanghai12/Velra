#!/usr/bin/env node
// Launcher for `npm i -g velra` and `npx velra`.
//
// Velra is a native binary, not a Node program: `velra enable` registers the
// *binary's* absolute path with Claude Code, so hooks never start Node. This
// script's only job is to make sure the right binary for this host exists on
// disk, then exec it.
//
// It resolves the binary in this order:
//
//   1. $VELRA_BINARY                     — explicit override
//   2. bin/velra[.exe] next to this file — vendored / offline install
//   3. the download cache                — a previous run put it there
//   4. GitHub Releases                   — downloaded, checksum-verified, cached
//
// There are no npm dependencies and no postinstall script. The download
// happens on first use, which is also the first time the user has asked for
// anything to happen.
//
// Environment:
//   VELRA_BINARY         absolute path to a velra binary; used as-is
//   VELRA_HOME           state/cache root (default: ~/.velra)
//   VELRA_VERSION        release to fetch (default: this package's version)
//   VELRA_REPO           owner/name on GitHub (default: aryanghai12/velra)
//   VELRA_DOWNLOAD_BASE  archive source: an https:// base URL or a local
//                        directory holding the release archives
//   VELRA_NO_DOWNLOAD=1  never reach the network; fail if not already cached

"use strict";

const { spawnSync } = require("node:child_process");
const crypto = require("node:crypto");
const fs = require("node:fs");
const https = require("node:https");
const http = require("node:http");
const os = require("node:os");
const path = require("node:path");
const zlib = require("node:zlib");
const { URL } = require("node:url");

const PKG_VERSION = require("../package.json").version;
const REPO = process.env.VELRA_REPO || "aryanghai12/velra";
const VERSION = (process.env.VELRA_VERSION || PKG_VERSION).replace(/^v/, "");
const HOMEPAGE = "https://github.com/aryanghai12/velra";
const RELEASES = HOMEPAGE + "/releases";

// Release targets, matching the artifacts produced by .github/workflows/release.yml.
const TARGETS = {
  "darwin arm64": { triple: "aarch64-apple-darwin", fmt: "tar.gz" },
  "darwin x64": { triple: "x86_64-apple-darwin", fmt: "tar.gz" },
  "linux x64": { triple: "x86_64-unknown-linux-musl", fmt: "tar.gz" },
  "linux arm64": { triple: "aarch64-unknown-linux-musl", fmt: "tar.gz" },
  "win32 x64": { triple: "x86_64-pc-windows-msvc", fmt: "zip" },
  "win32 arm64": { triple: "aarch64-pc-windows-msvc", fmt: "zip" },
};

const EXE = process.platform === "win32" ? "velra.exe" : "velra";

class VelraError extends Error {}

function fail(message) {
  throw new VelraError(message);
}

function target() {
  const key = `${process.platform} ${process.arch}`;
  const hit = TARGETS[key];
  if (!hit) {
    fail(
      `velra: no prebuilt binary for ${key}.\n` +
        `Supported: ${Object.keys(TARGETS).join(", ")}.\n` +
        `Build from source instead: cargo install velra\n` +
        `Or open an issue: ${HOMEPAGE}/issues`
    );
  }
  return hit;
}

function velraHome() {
  if (process.env.VELRA_HOME) return process.env.VELRA_HOME;
  const home = os.homedir();
  if (!home) fail("velra: cannot determine your home directory; set VELRA_HOME");
  return path.join(home, ".velra");
}

// A stable per-version path. `velra enable` writes this path into
// ~/.claude/settings.json, so it must not move between runs.
function cachedBinary(triple) {
  return path.join(velraHome(), "cache", `v${VERSION}`, triple, EXE);
}

function isExecutableFile(p) {
  try {
    return fs.statSync(p).isFile();
  } catch {
    return false;
  }
}

// ---------------------------------------------------------------- downloading

function get(url, redirectsLeft = 5) {
  return new Promise((resolve, reject) => {
    let parsed;
    try {
      parsed = new URL(url);
    } catch {
      reject(new VelraError(`velra: malformed URL: ${url}`));
      return;
    }
    const mod = parsed.protocol === "http:" ? http : https;
    const req = mod.get(
      parsed,
      { headers: { "User-Agent": `velra-npm/${PKG_VERSION}`, Accept: "*/*" } },
      (res) => {
        const status = res.statusCode || 0;
        if (status >= 300 && status < 400 && res.headers.location) {
          res.resume();
          if (redirectsLeft <= 0) {
            reject(new VelraError(`velra: too many redirects for ${url}`));
            return;
          }
          resolve(get(new URL(res.headers.location, parsed).toString(), redirectsLeft - 1));
          return;
        }
        if (status !== 200) {
          res.resume();
          reject(new VelraError(`velra: HTTP ${status} for ${url}`));
          return;
        }
        const chunks = [];
        res.on("data", (c) => chunks.push(c));
        res.on("end", () => resolve(Buffer.concat(chunks)));
        res.on("error", reject);
      }
    );
    req.on("error", (e) => reject(new VelraError(`velra: ${e.message} (${url})`)));
    req.setTimeout(120000, () => {
      req.destroy(new VelraError(`velra: timed out downloading ${url}`));
    });
  });
}

async function fetchAsset(base, name) {
  if (/^https?:\/\//.test(base)) return get(`${base}/${name}`);
  const local = path.join(base, name);
  if (!fs.existsSync(local)) fail(`velra: not found: ${local}`);
  return fs.readFileSync(local);
}

// ---------------------------------------------------------------- extraction
//
// Both extractors pull exactly one member out of the archive - the binary - and
// ignore everything else. Nothing is written from an archive-controlled path,
// so a crafted archive cannot escape the cache directory.

function fromTarGz(buf) {
  const tar = zlib.gunzipSync(buf);
  for (let off = 0; off + 512 <= tar.length; ) {
    const header = tar.subarray(off, off + 512);
    if (header.every((b) => b === 0)) break;
    const name = header.subarray(0, 100).toString("latin1").replace(/\0.*$/, "");
    const sizeField = header.subarray(124, 136).toString("latin1").replace(/[\0 ]/g, "");
    const size = sizeField ? parseInt(sizeField, 8) : 0;
    if (!Number.isFinite(size) || size < 0) fail("velra: corrupt tar header");
    const type = String.fromCharCode(header[156]);
    const dataStart = off + 512;
    if ((type === "0" || type === "\0") && path.posix.basename(name) === "velra") {
      return tar.subarray(dataStart, dataStart + size);
    }
    off = dataStart + Math.ceil(size / 512) * 512;
  }
  return null;
}

function fromZip(buf) {
  // Locate the end-of-central-directory record (it has a variable-length
  // comment, so scan backwards).
  let eocd = -1;
  for (let i = buf.length - 22; i >= 0 && i >= buf.length - 22 - 65535; i--) {
    if (buf.readUInt32LE(i) === 0x06054b50) {
      eocd = i;
      break;
    }
  }
  if (eocd < 0) fail("velra: not a zip archive");

  const count = buf.readUInt16LE(eocd + 10);
  let p = buf.readUInt32LE(eocd + 16);
  for (let i = 0; i < count; i++) {
    if (p + 46 > buf.length || buf.readUInt32LE(p) !== 0x02014b50) {
      fail("velra: corrupt zip central directory");
    }
    const method = buf.readUInt16LE(p + 10);
    const compressedSize = buf.readUInt32LE(p + 20);
    const nameLen = buf.readUInt16LE(p + 28);
    const extraLen = buf.readUInt16LE(p + 30);
    const commentLen = buf.readUInt16LE(p + 32);
    const localOffset = buf.readUInt32LE(p + 42);
    const name = buf.subarray(p + 46, p + 46 + nameLen).toString("utf8");

    if (path.posix.basename(name.replace(/\\/g, "/")) === "velra.exe") {
      if (buf.readUInt32LE(localOffset) !== 0x04034b50) fail("velra: corrupt zip entry");
      const lNameLen = buf.readUInt16LE(localOffset + 26);
      const lExtraLen = buf.readUInt16LE(localOffset + 28);
      const start = localOffset + 30 + lNameLen + lExtraLen;
      const raw = buf.subarray(start, start + compressedSize);
      if (method === 0) return raw;
      if (method === 8) return zlib.inflateRawSync(raw);
      fail(`velra: unsupported zip compression method ${method}`);
    }
    p += 46 + nameLen + extraLen + commentLen;
  }
  return null;
}

// ---------------------------------------------------------------- install

function installAtomically(dest, bytes) {
  const dir = path.dirname(dest);
  fs.mkdirSync(dir, { recursive: true });
  const staged = path.join(dir, `.velra-${process.pid}-${Date.now()}.tmp`);
  fs.writeFileSync(staged, bytes, { mode: 0o755 });
  try {
    fs.renameSync(staged, dest);
  } catch (e) {
    // On Windows a rename over a running executable fails. If a concurrent
    // install already produced the file, that is a win, not an error.
    fs.rmSync(staged, { force: true });
    if (!isExecutableFile(dest)) throw e;
  }
  if (process.platform !== "win32") {
    try {
      fs.chmodSync(dest, 0o755);
    } catch {
      /* best effort */
    }
  }
}

async function download(triple, fmt, dest) {
  if (process.env.VELRA_NO_DOWNLOAD === "1") {
    fail(
      `velra: no cached binary at ${dest} and VELRA_NO_DOWNLOAD=1.\n` +
        `Download it yourself from ${RELEASES}/tag/v${VERSION}`
    );
  }

  const archive = `velra-${triple}.${fmt}`;
  const base = process.env.VELRA_DOWNLOAD_BASE || `${RELEASES}/download/v${VERSION}`;

  process.stderr.write(`velra: downloading ${archive} (v${VERSION})...\n`);

  let blob;
  let sumFile;
  try {
    blob = await fetchAsset(base, archive);
    sumFile = await fetchAsset(base, `${archive}.sha256`);
  } catch (e) {
    fail(
      `${e.message}\n` +
        `velra: could not download the ${triple} binary.\n` +
        `Check your network, or install another way: ${HOMEPAGE}#quickstart`
    );
  }

  const expected = String(sumFile).trim().split(/\s+/)[0].toLowerCase();
  const actual = crypto.createHash("sha256").update(blob).digest("hex");
  if (!/^[0-9a-f]{64}$/.test(expected) || expected !== actual) {
    fail(
      `velra: checksum mismatch for ${archive}\n` +
        `  expected: ${expected}\n  actual:   ${actual}\n` +
        `Refusing to install. The download may be corrupt or tampered with.`
    );
  }

  const bytes = fmt === "zip" ? fromZip(blob) : fromTarGz(blob);
  if (!bytes || bytes.length === 0) fail(`velra: ${archive} did not contain ${EXE}`);

  installAtomically(dest, bytes);
  return dest;
}

// ---------------------------------------------------------------- resolve + run

async function resolveBinary() {
  const override = process.env.VELRA_BINARY;
  if (override) {
    if (!isExecutableFile(override)) fail(`velra: VELRA_BINARY is not a file: ${override}`);
    return override;
  }

  const vendored = path.join(__dirname, EXE);
  if (isExecutableFile(vendored)) return vendored;

  const { triple, fmt } = target();
  const cached = cachedBinary(triple);
  if (isExecutableFile(cached)) return cached;

  return download(triple, fmt, cached);
}

async function main() {
  const bin = await resolveBinary();
  const result = spawnSync(bin, process.argv.slice(2), { stdio: "inherit" });

  if (result.error) {
    const hint =
      result.error.code === "EACCES"
        ? "\nThe cached binary is not executable. Delete it and re-run to refetch:\n  " + bin
        : "";
    fail(`velra: could not run ${bin}: ${result.error.message}${hint}`);
  }
  if (result.signal) {
    // Report the signal the way a shell would.
    process.exit(128 + (os.constants.signals[result.signal] || 0));
  }
  process.exit(result.status === null ? 1 : result.status);
}

main().catch((e) => {
  const message = e instanceof VelraError ? e.message : `velra: ${e && e.stack ? e.stack : e}`;
  process.stderr.write(`${message}\n`);
  process.exit(1);
});
