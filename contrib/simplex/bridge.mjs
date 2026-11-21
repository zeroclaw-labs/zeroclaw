#!/usr/bin/env node
// Private stdio transport between the native adapter and the official SimpleX
// CLI. No API listener, account registration, or public bot address is created.
import WebSocket from 'ws';
import {createInterface} from 'node:readline';
import {randomUUID} from 'node:crypto';
import {homedir} from 'node:os';
import {join} from 'node:path';
import {once} from 'node:events';
import {openRtc} from './rtc.mjs';
import {Utterances} from './audio.mjs';
import {VoiceNotes} from './notes.mjs';
import {simplexTextMessages} from './format.mjs';
import {LiveTexts} from './live.mjs';

const pending = new Map();
let socket, call, invitation;
async function emit(value) {
  if (!process.stdout.write(JSON.stringify(value) + '\n')) await once(process.stdout, 'drain');
}
function rpc(cmd) {
  if (socket?.readyState !== WebSocket.OPEN) return Promise.reject(Error('simplex_disconnected'));
  if (pending.size >= 32) return Promise.reject(Error('simplex_pending_limit'));
  return new Promise((resolve, reject) => {
    const corrId = randomUUID();
    const timer = setTimeout(() => {pending.delete(corrId); reject(Error('simplex_rpc_timeout'));}, 20000);
    pending.set(corrId, {resolve, reject, timer});
    socket.send(JSON.stringify({corrId, cmd}), error => {
      if (error) {clearTimeout(timer); pending.delete(corrId); reject(Error('simplex_rpc_send_failed'));}
    });
  });
}
const notes = new VoiceNotes(rpc, emit);
const live = new LiveTexts(rpc);
async function endCall(notify = true) {
  const previous = call; call = undefined;
  if (!previous) return;
  const playback = await previous.rtc?.stop();
  await previous.rtc?.close();
  if (notify) await rpc(`/_call end @${previous.contact}`).catch(() => {});
  await emit({type: 'call_ended', contact: previous.contact, call_id: previous.id, scope: `${previous.id}/${previous.generation}`, playback});
}
function contactId(value) {
  if (!Number.isSafeInteger(value) || value <= 0) throw Error('simplex_invalid_contact');
  return String(value);
}
async function event(resp) {
  await notes.event(resp);
  switch (resp.type) {
    case 'newChatItems':
      for (const item of resp.chatItems ?? []) {
        if (item.chatInfo?.type !== 'direct' || item.chatItem?.chatDir?.type !== 'directRcv') continue;
        if (await notes.invitation(item)) continue;
        const body = item.chatItem.content;
        if (body?.type !== 'rcvMsgContent' || body.msgContent?.type !== 'text') continue;
        await emit({type: 'message', contact: contactId(item.chatInfo.contact.contactId),
          id: String(item.chatItem.meta.itemId), text: body.msgContent.text});
      }
      break;
    case 'callInvitation': {
      const incoming = resp.callInvitation;
      const contact = contactId(incoming.contact.contactId);
      // One call per bridge. Reject unencrypted/video calls before media setup.
      if (call || invitation || !incoming.sharedKey || incoming.callType?.media !== 'audio' || incoming.callType?.capabilities?.encryption !== true) {
        await rpc(`/_call reject @${contact}`); return;
      }
      invitation = {id: randomUUID(), contact, key: incoming.sharedKey};
      await emit({type: 'call_invitation', contact, call_id: invitation.id});
      break;
    }
    case 'callAnswer':
      if (call?.contact === String(resp.contact.contactId)) await call.rtc.command({type: 'answer', answer: resp.answer.rtcSession, iceCandidates: resp.answer.rtcIceCandidates});
      break;
    case 'callExtraInfo':
      if (call?.contact === String(resp.contact.contactId)) await call.rtc.command({type: 'ice', iceCandidates: resp.extraInfo.rtcIceCandidates});
      break;
    case 'callEnded':
      if (call?.contact === String(resp.contact.contactId)) await endCall(false);
      if (invitation?.contact === String(resp.contact.contactId)) invitation = undefined;
      break;
  }
}
async function interrupt(current, generation = randomUUID(), publish = true) {
  if (call !== current) return;
  current.generation = generation;
  const playback = await current.rtc.stop();
  const observation = {type: 'interrupted', contact: current.contact, call_id: current.id,
    scope: `${current.id}/${generation}`, playback};
  if (publish) await emit(observation);
  return observation;
}
async function accept(contact, callId, streaming) {
  if (!invitation || invitation.id !== callId || invitation.contact !== contact || call) throw Error('simplex_stale_invitation');
  const incoming = invitation; invitation = undefined;
  const current = {id: incoming.id, contact, generation: randomUUID(), rtc: undefined};
  call = current;
  const vad = new Utterances(() => {
    interrupt(current).catch(() => endCall());
  }, (audio, finished) => {
    if (call !== current) return;
    emit({type: 'utterance', contact, call_id: current.id, scope: `${current.id}/${current.generation}`, finished,
      audio: audio.toString('base64')}).catch(fatal);
  });
  current.rtc = await openRtc({
    assets: process.env.SIMPLEX_CALL_ASSETS ?? join(homedir(), '.local/share/zeroclaw-simplex'),
    executablePath: process.env.CHROMIUM_PATH,
    onPcm: encoded => {
      if (call !== current) return;
      if (streaming) return emit({type: 'call_audio', contact, call_id: current.id, audio: encoded});
      vad.push(Buffer.from(encoded, 'base64'));
    },
    onSignal: async ({resp, command}) => {
      if (call !== current || command) return; // command replies handled below
      try {
        if (resp.type === 'audio_error') {await endCall(); return;}
        if (resp.type === 'ice') await rpc(`/_call extra @${contact} ${JSON.stringify({rtcIceCandidates: resp.iceCandidates})}`);
        if (resp.type === 'connection') {
          const state = resp.state?.connectionState;
          if (['connected', 'disconnected', 'failed'].includes(state)) await rpc(`/_call status @${contact} ${state}`);
          if (state === 'connected') await emit({type: 'call_connected', contact, call_id: current.id});
          if (['failed', 'closed'].includes(state)) await endCall();
        }
      } catch {await endCall();}
    },
  });
  const {resp} = await current.rtc.command({type: 'start', media: 'audio', aesKey: incoming.key, relay: true});
  if (resp.type !== 'offer' || resp.capabilities?.encryption !== true) {await endCall(); throw Error('simplex_encryption_required');}
  await rpc(`/_call offer @${contact} ${JSON.stringify({callType: {media: 'audio', capabilities: {encryption: true}},
    rtcSession: {rtcSession: resp.offer, rtcIceCandidates: resp.iceCandidates}})}`);
}
async function command(message) {
  const {type, id} = message;
  let observation;
  try {
    if (type === 'init') {
      const url = new URL(message.websocket_url);
      if (url.protocol !== 'ws:' || !['127.0.0.1', '[::1]', 'localhost'].includes(url.hostname) || url.username || url.password) throw Error('simplex_local_api_required');
      socket = new WebSocket(url, {maxPayload: 8 * 1024 * 1024});
      await once(socket, 'open');
      let inbound = Promise.resolve(), queuedEvents = 0;
      socket.on('message', data => {
        let value;
        try {value = JSON.parse(data);} catch {return fatal();}
        const waiter = pending.get(value.corrId);
        if (waiter) {
          clearTimeout(waiter.timer); pending.delete(value.corrId);
          if (['chatCmdError', 'chatError'].includes(value.resp?.type)) waiter.reject(Object.assign(Error('simplex_command_failed'), {notDelivered:true}));
          else waiter.resolve(value.resp);
        } else if (!value.corrId) {
          if (queuedEvents >= 32) {fatal(); return;}
          queuedEvents++;
          inbound = inbound.then(() => event(value.resp ?? {})).finally(() => queuedEvents--).catch(fatal);
        }
      });
      socket.on('close', fatal); socket.on('error', fatal);
      setImmediate(() => notes.recover().then(() => live.discover(emit)).catch(fatal));
    } else if (type === 'ping') await rpc('/u');
    else if (type === 'receive_voice') await notes.receive(message);
    else if (type === 'send') {
      const contact = contactId(Number(message.contact));
      if (message.voice) await notes.send(contact, message.text, message.voice);
      else {
      for (const content of simplexTextMessages(message.text))
        await rpc(`/_send @${contact} json ${JSON.stringify([content])}`);
      }
    } else if (type === 'live_start') await live.start(message.draft, contactId(Number(message.contact)), message.text);
    else if (type === 'live_update' || type === 'live_status') await live.update(message.draft, contactId(Number(message.contact)), message.text, type === 'live_status');
    else if (type === 'live_flush') await live.flush(message.draft, contactId(Number(message.contact)), message.text, message.receipt);
    else if (type === 'live_finish') await live.finish(message.draft, contactId(Number(message.contact)), message.text);
    else if (type === 'live_cancel') await live.cancel(message.draft, contactId(Number(message.contact)), message.interrupted);
    else if (type === 'live_recover') await live.recover(contactId(Number(message.contact)), message.item_id, message.interrupted, message.placeholders);
    else if (type === 'accept') await accept(contactId(Number(message.contact)), message.call_id, message.streaming === true);
    else if (type === 'speech_start') {
      if (!call || call.id !== message.call_id || call.contact !== message.contact || typeof message.utterance_id !== 'string' || !/^[a-zA-Z0-9-]{1,128}$/.test(message.utterance_id)) throw Error('simplex_invalid_speech_start');
      observation = await interrupt(call, message.utterance_id, false);
    }
    else if (type === 'reject') {
      if (invitation?.id === message.call_id) {await rpc(`/_call reject @${invitation.contact}`); invitation = undefined;}
    } else if (type === 'audio') {
      if (!call || message.scope !== `${call.id}/${call.generation}` || message.contact !== call.contact) throw Object.assign(Error('simplex_stale_call_audio'), {notDelivered: true});
      if (message.text !== undefined && (typeof message.text !== 'string' || message.text.length > 16000)) throw Error('simplex_invalid_speech_text');
      await call.rtc.play(message.audio, {scope: message.scope, segment: message.id, text: message.text ?? ''});
    } else if (type === 'stop_audio') {
      if (call?.contact === message.contact && message.scope === `${call.id}/${call.generation}`) await call.rtc.stop();
    } else if (type === 'hangup') {
      if (!message.call_id || (call?.id === message.call_id && call?.contact === message.contact)) await endCall();
    }
    else throw Error('simplex_unknown_bridge_command');
    await emit({type: 'result', id, ok: true, observation});
  } catch (error) {
    if (type === 'accept') await endCall().catch(() => {});
    await emit({type: 'result', id, ok: false, not_delivered: error.notDelivered === true, no_resend: error.noResend === true});
  }
}
let shuttingDown = false;
async function fatal() {
  if (shuttingDown) return;
  shuttingDown = true;
  for (const {reject, timer} of pending.values()) {clearTimeout(timer); reject(Error('simplex_disconnected'));}
  pending.clear();
  await endCall(false).catch(() => {});
  await notes.close().catch(() => {});
  socket?.terminate(); process.exitCode = 1;
  process.stdin.destroy();
}
const input = createInterface({input: process.stdin, crlfDelay: Infinity});
// RPC correlations own ordering. In particular, playback backpressure must not
// block pings, hangups, text delivery, or an interruption on the same process.
let running = 0;
input.on('line', line => {
  if (line.length > 8 * 1024 * 1024) {fatal(); return;}
  if (running >= 32) {fatal(); return;}
  running++;
  try {command(JSON.parse(line)).catch(fatal).finally(() => running--);}
  catch {fatal();}
});
input.on('close', () => fatal());
process.on('SIGTERM', fatal); process.on('SIGINT', fatal);
