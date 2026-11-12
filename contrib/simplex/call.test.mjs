// Opt-in public TURN test with the real bridge and a simulated CLI signaling
// endpoint. No user accounts, phone contacts, model calls, or recordings.
import {test} from 'node:test';
import assert from 'node:assert/strict';
import {WebSocketServer} from 'ws';
import {spawn} from 'node:child_process';
import {createInterface} from 'node:readline';
import {once} from 'node:events';
import {randomBytes} from 'node:crypto';
import {fileURLToPath} from 'node:url';
import {homedir} from 'node:os';
import {join} from 'node:path';
import {openRtc} from './rtc.mjs';
import {wav} from './audio.mjs';

for (const streaming of [false, true]) test(`real bridge ${streaming ? 'streams PCM and returns an ordered playback receipt' : 'batches speech'} through an encrypted call and blocks stale audio`, {skip: !process.env.SIMPLEX_RELAY_TEST, timeout: 45000}, async () => {
  const server = new WebSocketServer({host: '127.0.0.1', port: 0}); await once(server, 'listening');
  const child = spawn(process.execPath, [fileURLToPath(new URL('./bridge.mjs', import.meta.url))], {stdio: ['pipe', 'pipe', 'ignore']});
  const lines = createInterface({input: child.stdout}); const queue = [];
  lines.on('line', line => queue.push(JSON.parse(line)));
  async function next(type, id) {
    const deadline = Date.now() + 25000;
    while (Date.now() < deadline) {
      const index = queue.findIndex(v => v.type === type && (!id || v.id === id));
      if (index >= 0) return queue.splice(index, 1)[0];
      await new Promise(resolve => setTimeout(resolve, 30));
    }
    throw Error(`missing bridge event: ${type}`);
  }
  const send = value => child.stdin.write(JSON.stringify(value) + '\n');
  let phone, socket; let audible = 0; const statuses = [];
  try {
    send({type: 'init', id: 'init', websocket_url: `ws://127.0.0.1:${server.address().port}`});
    [socket] = await once(server, 'connection');
    assert.equal((await next('result', 'init')).ok, true);
    const emit = resp => socket.send(JSON.stringify({resp}));
    const key = randomBytes(32).toString('base64').replaceAll('+', '-').replaceAll('/', '_');
    phone = await openRtc({assets: process.env.SIMPLEX_CALL_ASSETS ?? join(homedir(), '.local/share/zeroclaw-simplex'), executablePath: process.env.CHROMIUM_PATH,
      onSignal: ({resp, command}) => {
        if (!command && resp.type === 'ice') emit({type: 'callExtraInfo', contact: {contactId: 17}, extraInfo: {rtcIceCandidates: resp.iceCandidates}});
      }, onPcm: encoded => {
        const pcm = Buffer.from(encoded, 'base64');
        for (let i = 0; i < pcm.length; i += 2) if (Math.abs(pcm.readInt16LE(i)) > 100) audible++;
      }});
    await phone.command({type: 'capabilities', media: 'audio'});
    let signaling = Promise.resolve();
    socket.on('message', data => {
      const {corrId, cmd} = JSON.parse(data);
      socket.send(JSON.stringify({corrId, resp: {type: 'cmdOk'}}));
      signaling = signaling.then(async () => {
        if (cmd.startsWith('/_call offer @17 ')) {
          const offer = JSON.parse(cmd.slice('/_call offer @17 '.length));
          const {resp} = await phone.command({type: 'offer', media: 'audio', aesKey: key, relay: true,
            offer: offer.rtcSession.rtcSession, iceCandidates: offer.rtcSession.rtcIceCandidates});
          assert.equal(resp.type, 'answer', resp.message);
          emit({type: 'callAnswer', contact: {contactId: 17}, answer: {rtcSession: resp.answer, rtcIceCandidates: resp.iceCandidates}});
        } else if (cmd.startsWith('/_call extra @17 ')) {
          const extra = JSON.parse(cmd.slice('/_call extra @17 '.length));
          await phone.command({type: 'ice', iceCandidates: extra.rtcIceCandidates});
        } else if (cmd.startsWith('/_call status @17 ')) statuses.push(cmd);
      });
    });
    emit({type: 'callInvitation', callInvitation: {contact: {contactId: 17}, sharedKey: key, callType: {media: 'audio', capabilities: {encryption: true}}}});
    const invitation = await next('call_invitation'); assert.ok(!JSON.stringify(invitation).includes(key));
    send({type: 'accept', id: 'accept', contact: '17', call_id: invitation.call_id, streaming});
    assert.equal((await next('result', 'accept')).ok, true);
    await next('call_connected'); await signaling;
    assert.ok(statuses.includes('/_call status @17 connected'));
    const tone = Buffer.alloc(32000);
    for (let i = 0; i < tone.length / 2; i++) tone.writeInt16LE(Math.round(6000 * Math.sin(i * Math.PI * 880 / 16000)), i * 2);
    const audio = wav(tone).toString('base64');
    await phone.play(audio);
    let utterance;
    if (streaming) {
      const frame = await next('call_audio');
      assert.equal(frame.call_id, invitation.call_id);
      assert.equal(Buffer.from(frame.audio, 'base64').length, 2048);
      assert.notEqual(Buffer.from(frame.audio, 'base64').toString('ascii',0,4), 'RIFF');
      send({type:'speech_start',id:'onset1',contact:'17',call_id:invitation.call_id,utterance_id:'u1'});
      const result = await next('result','onset1');
      assert.equal(result.ok,true);
      utterance = result.observation;
      assert.equal(utterance.scope, `${invitation.call_id}/u1`);
      assert.equal(utterance.playback.interrupted, false);
    } else {
      utterance = await next('utterance');
      assert.equal(utterance.finished, true);
      assert.equal(Buffer.from(utterance.audio, 'base64').toString('ascii', 0, 4), 'RIFF');
    }
    const longTone = wav(Buffer.concat(Array(10).fill(tone))).toString('base64');
    send({type: 'audio', id: 'audio', contact: '17', scope: utterance.scope, audio:longTone, text:'A longer answer that has already finished generating.'});
    assert.equal((await next('result', 'audio')).ok, true);
    const deadline = Date.now() + 5000;
    while (audible < 4000 && Date.now() < deadline) await new Promise(resolve => setTimeout(resolve, 50));
    assert.ok(audible > 4000);
    queue.splice(0, queue.length); await phone.play(audio);
    if (streaming) {
      await next('call_audio');
      send({type:'speech_start',id:'onset2',contact:'17',call_id:invitation.call_id,utterance_id:'u2'});
      const result = await next('result','onset2');
      assert.equal(result.ok,true);
      assert.equal(result.observation.playback.interrupted,true);
      assert.equal(result.observation.playback.partial.text,'A longer answer that has already finished generating.');
      assert.ok(result.observation.playback.partial.played_ms > 0);
      assert.ok(result.observation.playback.partial.played_ms < 10000);
      assert.ok(!queue.some(event => ['utterance','interrupted'].includes(event.type)), 'streaming onset returns its observation in the same RPC');
    } else await next('interrupted');
    send({type: 'audio', id: 'stale', contact: '17', scope: utterance.scope, audio});
    const rejected = await next('result', 'stale');
    assert.equal(rejected.ok, false); assert.equal(rejected.not_delivered, true);
    send({type: 'hangup', id: 'hangup'}); assert.equal((await next('result', 'hangup')).ok, true);
    await next('call_ended');
  } finally {child.stdin.end(); child.kill('SIGTERM'); await phone?.close(); for (const socket of server.clients) socket.terminate(); await new Promise(resolve => server.close(resolve));}
});
