#!/usr/bin/env node
'use strict';

// Postinstall: download the platform-specific zeroclaw binary from the
// matching GitHub Release into ./native/. Pattern adapted from claurst's
// npm wrapper (npm/install.js); adjusted for zeroclaw's release asset
// naming and the ZEROCLAW_DOWNLOAD_BASE override.

const https = require('https');
const http = require('http');
const fs = require('fs');
const path = require('path');
const os = require('os');
const { execFileSync } = require('child_process');

const pkg = require('./package.json');
const VERSION = pkg.version;
const REPO = process.env.ZEROCLAW_RELEASE_REPO || 'zeroclaw-labs/zeroclaw';
const BASE_URL =
  process.env.ZEROCLAW_DOWNLOAD_BASE ||
  `https://github.com/${REPO}/releases/download/v${VERSION}`;
const NATIVE_DIR = path.join(__dirname, 'native');

function getPlatform() {
  const platform = process.platform;
  const arch = process.arch;

  if (platform === 'win32' && arch === 'x64') {
    return { artifact: 'zeroclaw-windows-x86_64', ext: '.exe', archive: '.zip' };
  }
  if (platform === 'linux' && arch === 'x64') {
    return { artifact: 'zeroclaw-linux-x86_64', ext: '', archive: '.tar.gz' };
  }
  if (platform === 'linux' && arch === 'arm64') {
    return { artifact: 'zeroclaw-linux-aarch64', ext: '', archive: '.tar.gz' };
  }
  if (platform === 'darwin' && arch === 'x64') {
    return { artifact: 'zeroclaw-macos-x86_64', ext: '', archive: '.tar.gz' };
  }
  if (platform === 'darwin' && arch === 'arm64') {
    return { artifact: 'zeroclaw-macos-aarch64', ext: '', archive: '.tar.gz' };
  }
  throw new Error(
    `Unsupported platform: ${platform}/${arch}.\n` +
      `Install manually from: https://github.com/${REPO}/releases/tag/v${VERSION}`,
  );
}

function download(url, dest) {
  return new Promise((resolve, reject) => {
    const file = fs.createWriteStream(dest);
    const get = url.startsWith('https') ? https : http;
    get
      .get(url, (res) => {
        if (res.statusCode === 301 || res.statusCode === 302) {
          file.close();
          try {
            fs.unlinkSync(dest);
          } catch (_) {}
          download(res.headers.location, dest).then(resolve).catch(reject);
          return;
        }
        if (res.statusCode !== 200) {
          file.close();
          try {
            fs.unlinkSync(dest);
          } catch (_) {}
          reject(new Error(`HTTP ${res.statusCode} downloading ${url}`));
          return;
        }
        res.pipe(file);
        file.on('finish', () => file.close(resolve));
        file.on('error', (err) => {
          try {
            fs.unlinkSync(dest);
          } catch (_) {}
          reject(err);
        });
      })
      .on('error', (err) => {
        try {
          fs.unlinkSync(dest);
        } catch (_) {}
        reject(err);
      });
  });
}

async function main() {
  // Allow opting out — useful in CI/Docker where the binary is mounted in.
  if (process.env.ZEROCLAW_SKIP_POSTINSTALL === '1') {
    console.log('zeroclaw: ZEROCLAW_SKIP_POSTINSTALL=1 — skipping binary download.');
    return;
  }

  const { artifact, ext, archive } = getPlatform();
  const archiveName = `${artifact}${archive}`;
  const url = `${BASE_URL}/${archiveName}`;
  const tmpPath = path.join(os.tmpdir(), `zeroclaw-install-${process.pid}${archive}`);
  const binaryDest = path.join(NATIVE_DIR, `zeroclaw${ext}`);

  if (fs.existsSync(binaryDest)) {
    console.log('zeroclaw: native binary already present, skipping download.');
    return;
  }

  fs.mkdirSync(NATIVE_DIR, { recursive: true });

  console.log(`zeroclaw: downloading v${VERSION} for ${process.platform}/${process.arch}`);
  console.log(`          ${url}`);
  await download(url, tmpPath);

  console.log('zeroclaw: extracting...');
  if (archive === '.zip') {
    execFileSync('powershell', [
      '-NoProfile',
      '-NonInteractive',
      '-Command',
      `Expand-Archive -Force -Path "${tmpPath}" -DestinationPath "${NATIVE_DIR}"`,
    ]);
  } else {
    execFileSync('tar', ['-xzf', tmpPath, '-C', NATIVE_DIR]);
  }

  try {
    fs.unlinkSync(tmpPath);
  } catch (_) {}

  if (!fs.existsSync(binaryDest)) {
    throw new Error(`Extraction succeeded but binary not found at ${binaryDest}`);
  }

  if (ext === '') {
    fs.chmodSync(binaryDest, 0o755);
  }

  console.log('zeroclaw: ready — run `zeroclaw` to start.');
}

main().catch((err) => {
  console.error(`\nzeroclaw install failed: ${err.message}`);
  console.error(`Manual install: https://github.com/${REPO}/releases/tag/v${VERSION}\n`);
  process.exit(1);
});
