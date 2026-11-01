"""Run the native channel boundary proof against a disposable local Prosody.

Development/CI entrypoint only. Fixed loopback endpoints, generated accounts and
certificate, private temporary state, and cleanup of only the child we start.
"""
import argparse
import json
import os
import pathlib
import secrets
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=pathlib.Path, required=True)
    options = parser.parse_args()
    if not sys.platform.startswith('linux'):
        parser.error('The native encrypted state fixture requires Linux')
    prosody = shutil.which('prosody')
    if not prosody or not shutil.which('openssl'):
        parser.error('Install Prosody and OpenSSL before running this fixture')
    for port in (15222, 15223):
        with socket.socket() as probe:
            if probe.connect_ex(('127.0.0.1', port)) == 0:
                parser.error('Synthetic fixture port occupied; refusing to interfere')
    root = pathlib.Path(__file__).resolve().parent
    output = options.output.resolve()
    if output.exists():
        parser.error('Choose a new untracked output report path')
    with tempfile.TemporaryDirectory(prefix='zeroclaw-native-prosody-', dir='/var/tmp') as temp:
        server = pathlib.Path(temp)
        server.chmod(0o700)
        certs = server / 'certs'
        certs.mkdir(mode=0o700)
        def openssl(*args):
            subprocess.run(['openssl', *args], check=True,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

        openssl(
            'req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1',
            '-subj', '/CN=ZeroClaw disposable fixture CA',
            '-addext', 'basicConstraints=critical,CA:TRUE',
            '-addext', 'keyUsage=critical,keyCertSign,cRLSign',
            '-keyout', str(certs / 'ca-key.pem'), '-out', str(certs / 'cert.pem'))
        openssl('req', '-new', '-newkey', 'rsa:2048', '-nodes',
                '-subj', '/CN=example.test', '-keyout', str(certs / 'key.pem'),
                '-out', str(certs / 'server.csr'))
        extensions = certs / 'server.ext'
        extensions.write_text('basicConstraints=critical,CA:FALSE\n'
                              'keyUsage=critical,digitalSignature,keyEncipherment\n'
                              'extendedKeyUsage=serverAuth\n'
                              'subjectAltName=DNS:example.test\n', encoding='utf-8')
        openssl('x509', '-req', '-in', str(certs / 'server.csr'),
                '-CA', str(certs / 'cert.pem'), '-CAkey', str(certs / 'ca-key.pem'),
                '-CAcreateserial', '-days', '1', '-extfile', str(extensions),
                '-out', str(certs / 'server.pem'))
        (certs / 'ca-key.pem').chmod(0o600)
        (certs / 'key.pem').chmod(0o600)
        accounts = {name: secrets.token_urlsafe(24) for name in ('bot', 'bob', 'alice', 'mallory')}
        with open(server / 'fixture-accounts.json', 'x', opener=lambda p, f: os.open(p, f, 0o600)) as file:
            json.dump(accounts, file)
        account_dir = server / 'data/example%2etest/accounts'
        account_dir.mkdir(parents=True, mode=0o700)
        for name, password in accounts.items():
            with open(account_dir / (name + '.dat'), 'x', opener=lambda p, f: os.open(p, f, 0o600)) as file:
                file.write('return { password = ' + json.dumps(password) + ' };\n')
        config = '\n'.join([
            'daemonize = false',
            'pidfile = ' + json.dumps(str(server / 'prosody.pid')),
            'data_path = ' + json.dumps(str(server / 'data')),
            'interfaces = { "127.0.0.1" }',
            'c2s_ports = { 15222 }', 'c2s_direct_tls_ports = { 15223 }',
            's2s_ports = {}', 's2s_direct_tls_ports = {}',
            'http_ports = {}', 'https_ports = {}', 'component_ports = {}',
            'modules_enabled = { "disco"; "roster"; "saslauth"; "tls"; "pep"; "pubsub"; "private"; "ping"; }',
            'modules_disabled = { "s2s"; "register" }',
            'authentication = "internal_plain"', 'allow_registration = false',
            'c2s_require_encryption = true',
            'log = { error = ' + json.dumps(str(server / 'error.log')) + ' }',
            'ssl = { key = ' + json.dumps(str(certs / 'key.pem')) + '; certificate = '
            + json.dumps(str(certs / 'server.pem')) + '; }',
            'VirtualHost "example.test"',
        ])
        config_path = server / 'prosody.cfg.lua'
        config_path.write_text(config + '\n', encoding='utf-8')
        with (server / 'process.log').open('wb') as log:
            process = subprocess.Popen([prosody, '--config', str(config_path), '-F'],
                                       stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        try:
            for _ in range(200):
                if process.poll() is not None:
                    raise RuntimeError('Synthetic Prosody exited during startup')
                with socket.socket() as probe:
                    if probe.connect_ex(('127.0.0.1', 15222)) == 0:
                        break
                time.sleep(0.05)
            else:
                raise RuntimeError('Synthetic Prosody readiness timeout')
            subprocess.run([sys.executable, str(root / 'integration.py'),
                            '--server-root', str(server), '--output', str(output)], check=True)
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=8)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=3)


if __name__ == '__main__':
    main()
