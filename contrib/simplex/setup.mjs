#!/usr/bin/env node
// Download upstream artifacts, never execute a downloaded installation script.
// Keep the separately installed SimpleX AGPL assets and their license together.
import {createHash} from 'node:crypto';
import {mkdir, writeFile, rename, chmod, readFile} from 'node:fs/promises';
import {homedir} from 'node:os';
import {join} from 'node:path';
const target = process.argv[2] ?? join(homedir(), '.local/share/zeroclaw-simplex');
const version = 'v7.0.3';
const artifacts = [
  ['call.js', `https://raw.githubusercontent.com/simplex-chat/simplex-chat/${version}/apps/multiplatform/common/src/commonMain/resources/assets/www/call.js`, '0f17cb0609f54c6713be0efd741934d2037b814357ae58fe1ed1809f5cd8b4f8'],
  ['lz-string.min.js', `https://raw.githubusercontent.com/simplex-chat/simplex-chat/${version}/apps/multiplatform/common/src/commonMain/resources/assets/www/lz-string.min.js`, '95f4d1cbf099f57161b664bc048426ec3df92637801a4c79116e83315aa787e7'],
  ['LICENSE', `https://github.com/simplex-chat/simplex-chat/releases/download/${version}/LICENSE`, '8486a10c4393cee1c25392769ddd3b2d6c242d6ec7928e1414efff7dfb2f07ef'],
];
if (process.platform === 'linux' && process.arch === 'x64') artifacts.push(['simplex-chat', `https://github.com/simplex-chat/simplex-chat/releases/download/${version}/simplex-chat-ubuntu-24_04-x86_64`, '895fb14cfaa662d1c0947f2f871141fc89680fe3e70bff64c366cbe9e59aa4f0']);
process.umask(0o077);
await mkdir(target, {recursive: true, mode: 0o700});
for (const [name, url, hash] of artifacts) {
  const path = join(target, name);
  let data;
  try {data = await readFile(path);} catch (error) {if (error.code !== 'ENOENT') throw error;}
  if (!data || createHash('sha256').update(data).digest('hex') !== hash) {
    const response = await fetch(url);
    if (!response.ok) throw Error(`artifact_download_failed: ${name}`);
    data = Buffer.from(await response.arrayBuffer());
    if (createHash('sha256').update(data).digest('hex') !== hash) throw Error(`artifact_digest_mismatch: ${name}`);
    await writeFile(path + '.next', data, {mode: 0o600}); await rename(path + '.next', path);
  }
  await chmod(path, name === 'simplex-chat' ? 0o700 : 0o600);
}
console.log(`Verified SimpleX ${version} artifacts`);
