"""Opt-in real-server bridge checks using disposable accounts, never real peers.

Fixture JSON: passwords for bot/alice/mallory. See the XMPP guide for the isolated
Prosody server requirements. All accounts are on example.test, loopback only.
"""
import argparse
import asyncio
import json
import os
from pathlib import Path
import sqlite3
import tempfile


class Worker:
    def __init__(self, process, state):
        self.process, self.state = process, state

    async def event(self, kind, timeout=20):
        async def read():
            while line := await self.process.stdout.readline():
                data = json.loads(line)
                if data['type'] == kind:
                    return data
                if data['type'] == 'fatal':
                    raise AssertionError('Bridge failed')
            raise AssertionError('Bridge exited before expected event')
        return await asyncio.wait_for(read(), timeout)

    async def send(self, target, body, expected=True):
        command = dict(type='send', id='test', to=target, body=body)
        self.process.stdin.write((json.dumps(command)+'\n').encode())
        await self.process.stdin.drain()
        result = await self.event('sent', 55)
        assert result['ok'] is expected, result

    def identity(self):
        with sqlite3.connect(self.state/'omemo.sqlite3') as db:
            return json.loads(db.execute("select value from state where key='public/identity'").fetchone()[0])

    async def close(self):
        if self.process.returncode is None:
            self.process.terminate()
        await self.process.wait()


async def test(args):
    passwords=json.loads(Path(args.fixture).read_text())
    source=Path(args.bridge).read_text()
    env=os.environ.copy();env['SSL_CERT_FILE']=args.certificate
    root=Path(tempfile.mkdtemp(prefix='xmpp-channel-integration-'))
    workers=[]

    async def start(name, protocol='direct_tls', omemo=False, pins=(), state=None, trusted_ca=True):
        cfg=dict(username=name+'@example.test',server='127.0.0.1',port=15223 if protocol=='direct_tls' else 15222,
                 protocol=protocol,password=passwords[name],resource='integration',max_message_chars=16000,
                 omemo_enabled=omemo,omemo_trusted_devices=list(pins),connect_timeout_secs=20)
        child_env=env.copy()
        if not trusted_ca:child_env.pop('SSL_CERT_FILE',None)
        p=await asyncio.create_subprocess_exec(args.python,'-I','-u','-c',source,stdin=asyncio.subprocess.PIPE,
                                              stdout=asyncio.subprocess.PIPE,stderr=asyncio.subprocess.DEVNULL,env=child_env)
        state=state or root/(name+'-'+str(len(workers)))
        p.stdin.write((json.dumps(dict(config=cfg,storage=str(state),peers=[]))+'\n').encode());await p.stdin.drain()
        w=Worker(p,state);workers.append(w)
        return w

    try:
        bot=await start('bot');alice=await start('alice','starttls')
        await bot.event('online');await alice.event('online')
        await alice.send('bot@example.test','direct-and-starttls')
        message=await bot.event('message')
        assert message['body']=='direct-and-starttls' and message['from']=='alice@example.test' and not message['encrypted']
        await bot.close();await alice.close()
        print('PASS direct TLS / mandatory STARTTLS and normalized direct messages')

        invalid=await start('bot',trusted_ca=False)
        try:
            await invalid.event('online',5)
        except (AssertionError,asyncio.TimeoutError):pass
        else:raise AssertionError('Untrusted certificate accepted')
        await invalid.close()
        print('PASS untrusted TLS certificate rejected')

        bot=await start('bot',omemo=True);alice=await start('alice','starttls',True)
        await bot.event('online');await alice.event('online')
        identities=[bot.identity(),alice.identity()]
        pins=[i['jid']+' '+i['fingerprint'] for i in identities]
        old_states=[bot.state,alice.state]
        await bot.close();await alice.close()
        bot=await start('bot',omemo=True,pins=pins,state=old_states[0])
        alice=await start('alice','starttls',True,pins,state=old_states[1])
        await bot.event('online');await alice.event('online')
        assert [bot.identity(),alice.identity()]==identities
        await alice.send('bot@example.test','encrypted-request')
        message=await bot.event('message')
        assert message['body']=='encrypted-request' and message['encrypted']
        await bot.send('alice@example.test','encrypted-reply')
        assert (await alice.event('message'))['body']=='encrypted-reply'
        print('PASS OMEMO authenticated encryption both ways and persistent identity across restart')

        # Cache an explicit distrust decision, then add the pin and receive
        # before sending anything. A first outbound message must not be needed
        # to activate trust in an already-known device.
        await bot.close()
        bot=await start('bot',omemo=True,pins=[],state=old_states[0])
        await bot.event('online')
        await bot.send('alice@example.test','seed-distrust',expected=False)
        await bot.close()
        bot=await start('bot',omemo=True,pins=pins,state=old_states[0])
        await bot.event('online')
        await alice.send('bot@example.test','first-inbound-after-pin-added')
        assert (await bot.event('message'))['body']=='first-inbound-after-pin-added'
        print('PASS newly pinned cached-distrusted device receives before first outbound')

        await bot.close()
        bot=await start('bot',omemo=True,pins=[],state=old_states[0])
        await bot.event('online')
        await alice.send('bot@example.test','revoked-inbound')
        try:await bot.event('message',2)
        except asyncio.TimeoutError:pass
        else:raise AssertionError('Removed pin still admitted inbound')
        await bot.close()
        bot=await start('bot',omemo=True,pins=pins,state=old_states[0])
        await bot.event('online')
        print('PASS removed pin blocks inbound before first outbound')

        plain=await start('mallory');await plain.event('online')
        await plain.send('bot@example.test','must-not-enter-agent')
        try:await bot.event('message',2)
        except asyncio.TimeoutError:pass
        else:raise AssertionError('Plaintext admitted with OMEMO required')
        print('PASS plaintext ignored when OMEMO required')
        await plain.close()

        await alice.close()
        alice=await start('alice','starttls',True,[],old_states[1]);await alice.event('online')
        await alice.send('bot@example.test','must-not-downgrade',expected=False)
        try:await bot.event('message',2)
        except asyncio.TimeoutError:pass
        else:raise AssertionError('Untrusted send fell back to plaintext')
        print('PASS revoked trust blocks outbound without plaintext fallback')
    finally:
        for worker in workers:await worker.close()


if __name__=='__main__':
    parser=argparse.ArgumentParser()
    parser.add_argument('--fixture',required=True)
    parser.add_argument('--certificate',required=True)
    parser.add_argument('--python',default='python3')
    parser.add_argument('--bridge',default=str(Path(__file__).parent.parent/'src/xmpp_bridge.py'))
    asyncio.run(test(parser.parse_args()))
