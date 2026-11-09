import {test} from 'node:test';
import assert from 'node:assert/strict';
import {randomBytes} from 'node:crypto';
import {openRtc} from './rtc.mjs';
import {homedir} from 'node:os';
import {join} from 'node:path';

for (const wrongKey of [false, true]) test(wrongKey ? 'wrong encryption key prevents audible audio' : 'encrypted WebRTC transports decoded audio in both directions', {skip: !process.env.SIMPLEX_RTC_TEST}, async () => {
  const config = {assets: process.env.SIMPLEX_CALL_ASSETS ?? join(homedir(), '.local/share/zeroclaw-simplex'),
    executablePath: process.env.CHROMIUM_PATH};
  // Separate sandboxed browsers cannot rely on host mDNS reachability in an
  // Incus container. Exercise the same public TURN path as real phone calls.
  const received = [0, 0];
  const peers = [], ready = [false, false], pendingIce = [[], []];
  const key = randomBytes(32).toString('base64').replaceAll('+', '-').replaceAll('/', '_');
  try {
    for (let i = 0; i < 2; i++) peers.push(await openRtc({...config,
      onSignal: ({resp, command}) => {
        if (!command && resp.type === 'ice') {
          const target = 1-i;
          if (ready[target]) return peers[target].command({type: 'ice', iceCandidates: resp.iceCandidates});
          pendingIce[target].push(resp.iceCandidates);
        }
      }, onPcm: encoded => {
        const pcm = Buffer.from(encoded, 'base64');
        for (let j = 0; j < pcm.length; j += 2) if (Math.abs(pcm.readInt16LE(j)) > 100) received[i]++;
      }}));
    assert.equal((await peers[1].command({type: 'capabilities', media: 'audio'})).resp.capabilities.encryption, true);
    const offer = (await peers[0].command({type: 'start', media: 'audio', aesKey: key, relay: true})).resp;
    assert.equal(offer.type, 'offer', offer.message);
    assert.equal(offer.capabilities.encryption, true);
    const answer = (await peers[1].command({type: 'offer', media: 'audio', aesKey: wrongKey ? randomBytes(32).toString('base64').replaceAll('+', '-').replaceAll('/', '_') : key,
      offer: offer.offer, iceCandidates: offer.iceCandidates, relay: true})).resp;
    assert.equal(answer.type, 'answer', answer.message);
    assert.equal((await peers[0].command({type: 'answer', answer: answer.answer, iceCandidates: answer.iceCandidates})).resp.type, 'ok');
    // The test signaling fixture must preserve candidates that arrive before
    // the receiving peer has installed its remote session description.
    for (let i = 0; i < 2; i++) {
      ready[i] = true;
      for (const iceCandidates of pendingIce[i]) assert.equal((await peers[i].command({type:'ice',iceCandidates})).resp.type,'ok');
    }
    const connecting = Date.now() + 20000;
    while ((await Promise.all(peers.map(p => p.diagnostics()))).some(s => s.state !== 'connected') && Date.now() < connecting) await new Promise(resolve => setTimeout(resolve, 50));
    assert.ok((await Promise.all(peers.map(p => p.diagnostics()))).every(s => s.state === 'connected'), JSON.stringify(await Promise.all(peers.map(p => p.diagnostics()))));
    const diagnostics = await Promise.all(peers.map(p => p.diagnostics()));
    for (const diagnostic of diagnostics) {
      assert.equal(diagnostic.media.playbackSampleRate, 48000);
      assert.equal(diagnostic.media.recognitionSampleRate, 16000);
      assert.equal(diagnostic.media.contentHint, 'music');
      assert.ok(diagnostic.codecs.some(codec => codec.mimeType === 'audio/opus' && codec.clockRate === 48000));
    }
    await new Promise(resolve => setTimeout(resolve, 300));
    const samples = 16000;
    const wav = Buffer.alloc(44 + samples * 2);
    wav.write('RIFF'); wav.writeUInt32LE(wav.length - 8, 4); wav.write('WAVEfmt ', 8);
    wav.writeUInt32LE(16, 16); wav.writeUInt16LE(1, 20); wav.writeUInt16LE(1, 22);
    wav.writeUInt32LE(16000, 24); wav.writeUInt32LE(32000, 28); wav.writeUInt16LE(2, 32); wav.writeUInt16LE(16, 34);
    wav.write('data', 36); wav.writeUInt32LE(samples * 2, 40);
    for (let i = 0; i < samples; i++) wav.writeInt16LE(Math.round(8000 * Math.sin(i * 2 * Math.PI * 440 / 16000)), 44 + i * 2);
    await Promise.all(peers.map(peer => peer.play(wav.toString('base64'))));
    const deadline = Date.now() + (wrongKey ? 2000 : 6000);
    while (received.some(count => count < 4000) && Date.now() < deadline) await new Promise(resolve => setTimeout(resolve, 100));
    if (wrongKey) {
      assert.deepEqual(received, [0, 0]);
      assert.ok((await Promise.all(peers.map(p => p.diagnostics()))).every(s => s.stats.some(s => s.packetsSent > 0)));
    } else {
      assert.ok(received.every(count => count > 4000), `received audible samples: ${received}; ${JSON.stringify(await Promise.all(peers.map(p => p.diagnostics())))}`);
      await Promise.all(peers.map(peer => peer.stop()));
      const rate = 24000, seconds = 2;
      const high = Buffer.alloc(44 + rate * seconds * 2);
      wav.copy(high, 0, 0, 44);
      high.writeUInt32LE(high.length - 8, 4); high.writeUInt32LE(rate, 24);
      high.writeUInt32LE(rate * 2, 28); high.writeUInt32LE(rate * seconds * 2, 40);
      for (let i = 0; i < rate * seconds; i++) high.writeInt16LE(Math.round(4000 * Math.sin(i * 2 * Math.PI * 10000 / rate) + 4000 * Math.sin(i * 2 * Math.PI * 440 / rate)), 44 + i * 2);
      await Promise.all(peers.map(peer => peer.play(high.toString('base64'))));
      await new Promise(resolve => setTimeout(resolve, 600));
      const fullband = await Promise.all(peers.map(peer => peer.diagnostics({spectrum: true})));
      assert.ok(fullband.every(peer => peer.stats.some(stat => stat.targetBitrate === 64000)),
        'the uncongested test connection must use the requested Opus target');
      assert.ok(fullband.every(peer => peer.media.highBandPeakDb > -45),
        `10 kHz must survive encrypted transport before STT downsampling: ${JSON.stringify(fullband)}`);
    }
  } finally {await Promise.all(peers.map(peer => peer.close()));}
});


test('stopping playback cancels outstanding decode and leaves no late audio source', {
  skip: !process.env.SIMPLEX_RTC_TEST,
}, async () => {
  const peer = await openRtc({assets: process.env.SIMPLEX_CALL_ASSETS ?? join(homedir(), '.local/share/zeroclaw-simplex'),
    executablePath: process.env.CHROMIUM_PATH, onSignal: () => {}, onPcm: () => {}});
  try {
    const rate = 24000, samples = rate * 30;
    const audio = Buffer.alloc(44 + samples * 2);
    audio.write('RIFF'); audio.writeUInt32LE(audio.length - 8, 4); audio.write('WAVEfmt ', 8);
    audio.writeUInt32LE(16, 16); audio.writeUInt16LE(1, 20); audio.writeUInt16LE(1, 22);
    audio.writeUInt32LE(rate, 24); audio.writeUInt32LE(rate * 2, 28); audio.writeUInt16LE(2, 32); audio.writeUInt16LE(16, 34);
    audio.write('data', 36); audio.writeUInt32LE(samples * 2, 40);
    const outcome = peer.play(audio.toString('base64')).then(() => 'started', error => error);
    await peer.stop();
    const result = await outcome;
    assert.equal(result.notDelivered, true);
    const state = await peer.diagnostics();
    assert.equal(state.media.pendingPlaybacks, 0); assert.equal(state.media.playingSources, 0);
  } finally {await peer.close();}
});

test('playback cutoff survives completed generation and preserves completed segment receipts', {
  skip: !process.env.SIMPLEX_RTC_TEST,
}, async () => {
  const peer = await openRtc({assets: process.env.SIMPLEX_CALL_ASSETS ?? join(homedir(), '.local/share/zeroclaw-simplex'),
    executablePath: process.env.CHROMIUM_PATH, onSignal: () => {}, onPcm: () => {}});
  function silence(seconds) {
    const samples = Math.floor(seconds * 24000), audio = Buffer.alloc(44 + samples * 2);
    audio.write('RIFF');audio.writeUInt32LE(audio.length-8,4);audio.write('WAVEfmt ',8);
    audio.writeUInt32LE(16,16);audio.writeUInt16LE(1,20);audio.writeUInt16LE(1,22);
    audio.writeUInt32LE(24000,24);audio.writeUInt32LE(48000,28);audio.writeUInt16LE(2,32);audio.writeUInt16LE(16,34);
    audio.write('data',36);audio.writeUInt32LE(samples*2,40);return audio.toString('base64');
  }
  try {
    await peer.play(silence(0.1), {scope:'call/turn',segment:'first',text:'The completed opening sentence.'});
    await new Promise(resolve=>setTimeout(resolve,180));
    // The producer's completed promise does not mean the human finished listening.
    await peer.play(silence(10), {scope:'call/turn',segment:'second',text:'A long paragraph still being played.'});
    await new Promise(resolve=>setTimeout(resolve,200));
    const report = await peer.stop();
    assert.equal(report.interrupted,true);
    assert.deepEqual(report.completed,['The completed opening sentence.']);
    assert.equal(report.partial.segment,'second');
    assert.equal(report.partial.text,'A long paragraph still being played.');
    assert.ok(report.partial.played_ms>=100 && report.partial.played_ms<2000);
    assert.equal(report.partial.duration_ms,10000);
    assert.equal((await peer.stop()).interrupted,false,'the observation is consumed exactly once');
    assert.equal((await peer.diagnostics()).media.playingSources,0);
  } finally {await peer.close();}
});
