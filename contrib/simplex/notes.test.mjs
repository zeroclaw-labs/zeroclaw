import {test} from 'node:test';
import assert from 'node:assert/strict';
import {writeFile, readFile, stat, symlink} from 'node:fs/promises';
import {spawnSync} from 'node:child_process';
import {VoiceNotes, MAX_AUDIO} from './notes.mjs';

const makeAudio = () => {
  const result = spawnSync('ffmpeg', ['-v', 'error', '-f', 'lavfi', '-i', 'anullsrc=r=16000:cl=mono',
    '-t', '0.3', '-c:a', 'aac', '-movflags', 'frag_keyframe+empty_moov', '-f', 'mp4', 'pipe:1']);
  assert.equal(result.status, 0); return result.stdout;
};
// Inspect the on-disk playback contract independently of ffprobe's ability to
// reconstruct missing metadata from fragments.
function boxes(audio, start = 0, end = audio.length) {
  const result = [];
  while (start + 8 <= end) {
    let size = audio.readUInt32BE(start), header = 8;
    const type = audio.toString('ascii', start + 4, start + 8);
    if (size === 1) {size = Number(audio.readBigUInt64BE(start + 8)); header = 16;}
    if (size === 0) size = end - start;
    assert.ok(size >= header && start + size <= end);
    result.push({type, start: start + header, end: start + size}); start += size;
  }
  return result;
}
function packetHashes(audio) {
  const probe = spawnSync('ffprobe', ['-v', 'error', '-select_streams', 'a:0', '-show_packets',
    '-show_data_hash', 'sha256', '-of', 'json', 'pipe:0'], {input: audio});
  assert.equal(probe.status, 0);
  return JSON.parse(probe.stdout).packets.map(packet => packet.data_hash);
}
function assertFinalizedM4a(audio) {
  const top = boxes(audio), moov = top.find(box => box.type === 'moov');
  assert.ok(moov);
  assert.ok(top.findIndex(box => box.type === 'moov') < top.findIndex(box => box.type === 'mdat'));
  assert.ok(!top.some(box => box.type === 'moof'));
  const movie = boxes(audio, moov.start, moov.end);
  assert.ok(!movie.some(box => box.type === 'mvex'));
  const header = movie.find(box => box.type === 'mvhd');
  assert.equal(audio[header.start], 0);
  assert.ok(audio.readUInt32BE(header.start + 12) > 0);
  assert.ok(audio.readUInt32BE(header.start + 16) > 0, 'Android must see a nonzero movie duration');
  const track = movie.find(box => box.type === 'trak');
  const media = boxes(audio, track.start, track.end).find(box => box.type === 'mdia');
  const info = boxes(audio, media.start, media.end).find(box => box.type === 'minf');
  const table = boxes(audio, info.start, info.end).find(box => box.type === 'stbl');
  const samples = boxes(audio, table.start, table.end).find(box => box.type === 'stsz');
  assert.ok(audio.readUInt32BE(samples.start + 8) > 0, 'Android must see a populated sample index');
}

const item = (fileId = 7) => ({chatInfo: {type: 'direct', contact: {contactId: 17}},
  chatItem: {meta: {itemId: 42}, chatDir: {type: 'directRcv'}, content: {type: 'rcvMsgContent',
    msgContent: {type: 'voice', text: '', duration: 1}}, file: {fileId, fileName: 'voice.m4a', fileSize: 2000}}});
const receive = {contact: '17', message_id: '42', file_id: 7, filename: 'voice.m4a', max_duration: 600};

test('voice invitations require native admission before download; receive only opens its private owned file', async () => {
  const commands = [], events = [];
  const notes = new VoiceNotes(async cmd => {commands.push(cmd);}, async value => events.push(value));
  try {
    assert.equal(await notes.invitation(item()), true);
    assert.equal(commands.length, 0);
    assert.equal(events[0].type, 'voice_invitation');
    await notes.receive(receive);
    const path = commands[0].split(' encrypt=off ')[1];
    assert.ok(path.endsWith('/input.m4a'));
    assert.equal((await stat(notes.receiving.get(7).dir)).mode & 0o777, 0o700);
    const audio = makeAudio(); await writeFile(path, audio, {mode: 0o600});
    const complete = item(); complete.chatItem.file.fileSource = {filePath: path};
    await notes.event({type: 'rcvFileComplete', chatItem: complete});
    assert.equal(events[1].type, 'voice_note');
    assert.equal(events[1].id, '42'); assert.equal(events[1].contact, '17');
    assert.deepEqual(Buffer.from(events[1].audio, 'base64'), audio);
    assert.ok(events[1].duration > 0 && events[1].duration < 1);
    await assert.rejects(stat(path), {code: 'ENOENT'});
  } finally {await notes.close();}
});

test('unowned files, contact mismatch, oversized files, and symlinks never reach STT', async () => {
  for (const attack of ['path', 'contact', 'size', 'symlink']) {
    const events = []; const notes = new VoiceNotes(async () => {}, async value => events.push(value));
    try {
      await notes.receive(receive);
      const path = notes.receiving.get(7).path;
      if (attack === 'symlink') await symlink('/etc/passwd', path);
      else await writeFile(path, attack === 'size' ? Buffer.alloc(MAX_AUDIO + 1) : makeAudio());
      const complete = item(); complete.chatItem.file.fileSource = {filePath: attack === 'path' ? '/etc/passwd' : path};
      if (attack === 'contact') complete.chatInfo.contact.contactId = 18;
      await notes.event({type: 'rcvFileComplete', chatItem: complete});
      assert.deepEqual(events.map(e => e.type), ['voice_transfer_failed']);
      assert.equal(notes.receiving.size, 0);
      await assert.rejects(stat(path), {code: 'ENOENT'});
    } finally {await notes.close();}
  }
});

test('one batch creates text plus a native voice note; source survives ACK until encrypted upload completes', async () => {
  const commands = [], events = [];
  const notes = new VoiceNotes(async cmd => {commands.push(cmd); return {type: 'newChatItems', chatItems: []};}, async value => events.push(value));
  try {
    const audio = makeAudio(); await notes.send('17', '**The final reply.**', audio.toString('base64'));
    assert.equal(commands.length, 1);
    const batch = JSON.parse(commands[0].slice('/_send @17 json '.length));
    assert.equal(batch[0].msgContent.text, '*The final reply.*');
    assert.equal(batch[1].msgContent.type, 'voice'); assert.equal(batch[1].msgContent.duration, 1);
    const path = batch[1].fileSource.filePath;
    const uploaded = await readFile(path);
    assertFinalizedM4a(uploaded);
    assert.deepEqual(packetHashes(uploaded), packetHashes(audio), 'remuxing must preserve every AAC packet');
    assert.equal((await stat(path)).mode & 0o777, 0o600);
    const complete = item(); complete.chatItem.file.fileSource = {filePath: path};
    await notes.event({type: 'sndFileCompleteXFTP', chatItem: complete});
    await assert.rejects(stat(path), {code: 'ENOENT'}); assert.equal(notes.sending.size, 0);
    assert.equal(events.length, 0);
  } finally {await notes.close();}
});

test('failed probe is definitely unsent; lost CLI acceptance stays uncertain; shutdown removes artifacts', async () => {
  const notes = new VoiceNotes(async () => {throw Error('lost_acceptance');}, async () => {});
  try {
    await assert.rejects(notes.send('17', 'reply', Buffer.from('invalid audio').toString('base64')), error => error.notDelivered === true);
    await assert.rejects(notes.send('17', 'reply', makeAudio().toString('base64')), error => error.notDelivered === false);
    const paths = [...notes.sending.keys()]; assert.equal(paths.length, 1);
    await notes.close();
    for (const path of paths) await assert.rejects(stat(path), {code: 'ENOENT'});
  } finally {await notes.close();}
});

test('timeouts cancel admitted transfers, notify, and remove temporary files', async () => {
  const events = [], commands = [];
  const notes = new VoiceNotes(async cmd => commands.push(cmd), async value => events.push(value), {ttl: 25});
  try {
    await notes.receive(receive); const path = notes.receiving.get(7).path;
    await new Promise(resolve => setTimeout(resolve, 100));
    assert.ok(commands.includes('/fcancel 7'));
    assert.equal(notes.receiving.size, 0);
    assert.equal(events[0].type, 'voice_transfer_failed');
    await assert.rejects(stat(path), {code: 'ENOENT'});
  } finally {await notes.close();}
});

test('a delayed relay-policy reset fails promptly and preserves normal forwarded-note admission', async () => {
  const events = [], commands = [];
  const notes = new VoiceNotes(async cmd => commands.push(cmd), async event => events.push(event));
  try {
    const forwarded = item(); forwarded.chatItem.meta.itemForwarded = {type: 'unknown'};
    await notes.invitation(forwarded);
    assert.equal(events[0].type, 'voice_invitation');
    await notes.receive(receive);
    await notes.receive(receive);
    assert.equal(commands.length, 1, 'the same admitted transfer is idempotent');
    const path = notes.receiving.get(7).path;
    forwarded.chatItem.file.fileStatus = {type: 'rcvInvitation'};
    await notes.event({type: 'chatItemUpdated', chatItem: forwarded});
    assert.equal(notes.receiving.size, 0);
    assert.equal(events[1].type, 'voice_transfer_failed');
    await assert.rejects(stat(path), {code: 'ENOENT'});
  } finally {await notes.close();}
});

test('startup recovery emits only bounded recent pending metadata, leaving download to native authorization', async () => {
  const commands = [], events = [], now = Date.now();
  const pending = item().chatItem;
  pending.meta.createdAt = new Date(now - 1000).toISOString();
  pending.file.fileStatus = {type: 'rcvInvitation'};
  const complete = structuredClone(pending); complete.meta.itemId = 43; complete.file.fileStatus.type = 'rcvComplete';
  const old = structuredClone(pending); old.meta.itemId = 41; old.meta.createdAt = new Date(now - 7200000).toISOString();
  const notes = new VoiceNotes(async cmd => {
    commands.push(cmd);
    if (cmd === '/u') return {type: 'activeUser', user: {userId: 1}};
    if (cmd === '/_get chats 1 count=20') return {type: 'apiChats', chats: [{chatInfo: item().chatInfo}]};
    if (cmd === '/_get chat @17 count=20') return {type: 'apiChat', chat: {chatItems: [old, pending, complete]}};
    throw Error('unexpected download or unbounded history query');
  }, async event => events.push(event));
  try {
    await notes.recover(now);
    assert.equal(events.length, 1); assert.equal(events[0].id, '42');
    assert.equal(events[0].type, 'voice_invitation');
    assert.equal(commands.length, 3);
  } finally {await notes.close();}
});
