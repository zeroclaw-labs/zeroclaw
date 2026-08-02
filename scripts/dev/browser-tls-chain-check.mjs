#!/usr/bin/env node
import { webcrypto } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import process from 'node:process';
import vm from 'node:vm';

function parseArgs(argv) {
  const options = { ca: '', serverCert: '', clientCert: '', serverName: '127.0.0.1' };
  for (let index = 0; index < argv.length; index += 1) {
    const flag = argv[index];
    if (flag === '--ca') {
      options.ca = argv[++index] || '';
    } else if (flag === '--server-cert') {
      options.serverCert = argv[++index] || '';
    } else if (flag === '--client-cert') {
      options.clientCert = argv[++index] || '';
    } else if (flag === '--server-name') {
      options.serverName = argv[++index] || '';
    } else {
      throw new Error(`unknown argument: ${flag}`);
    }
  }
  for (const [name, value] of Object.entries(options)) {
    if (!value) {
      throw new Error(`missing --${name.replace(/[A-Z]/g, (letter) => `-${letter.toLowerCase()}`)}`);
    }
  }
  return options;
}

function loadTlsEngine() {
  const sourcePath = path.resolve(import.meta.dirname, '../../apps/zerorelay/src/frontdoor_tls_assets.rs');
  const source = fs.readFileSync(sourcePath, 'utf8');
  const match = source.match(/pub\(crate\) const TLS_ENGINE_JS: &str = r#"([\s\S]*)"#;\s*$/);
  if (!match) {
    throw new Error(`could not extract TLS engine from ${sourcePath}`);
  }
  const context = {
    ArrayBuffer,
    Date,
    Error,
    Map,
    Math,
    Number,
    Promise,
    RegExp,
    Set,
    String,
    TextDecoder,
    TextEncoder,
    Uint8Array,
    atob,
    btoa,
    console,
    crypto: webcrypto,
    self: {}
  };
  vm.runInNewContext(match[1], context, { filename: 'tls-engine.js' });
  return context.self.ZeroClawEnrollmentTls._internals;
}

async function rejects(promise, expected) {
  try {
    await promise;
  } catch (error) {
    if (String(error.message).includes(expected)) {
      return;
    }
    throw error;
  }
  throw new Error(`expected rejection containing: ${expected}`);
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const tls = loadTlsEngine();
  const caPem = fs.readFileSync(options.ca, 'utf8');
  const serverPem = fs.readFileSync(options.serverCert, 'utf8');
  const clientPem = fs.readFileSync(options.clientCert, 'utf8');
  const caDer = tls.pemBlocks(caPem, 'CERTIFICATE')[0];
  const serverDer = tls.pemBlocks(serverPem, 'CERTIFICATE')[0];
  const clientDer = tls.pemBlocks(clientPem, 'CERTIFICATE')[0];
  if (!caDer || !serverDer || !clientDer) {
    throw new Error('expected one CA, server, and client certificate');
  }

  await tls.verifyServerCertificateChain(serverDer, caPem, options.serverName);
  tls.assertCertificateAuthority(tls.parseCertificate(caDer));
  await rejects(
    tls.verifyServerCertificateChain(clientDer, caPem, options.serverName),
    'server authentication'
  );
  await rejects(
    tls.verifyServerCertificateChain(serverDer, caPem, 'not-the-daemon.example'),
    'does not match'
  );
  await rejects(
    tls.verifyServerCertificateChain(serverDer, caPem, options.serverName, 0),
    'validity period'
  );
  await rejects(
    Promise.resolve().then(() => tls.assertCertificateAuthority(tls.parseCertificate(clientDer))),
    'basic constraints'
  );
  console.log('browser TLS certificate policy ok');
}

main().catch((error) => {
  console.error(error.stack || error.message || error);
  process.exitCode = 1;
});
