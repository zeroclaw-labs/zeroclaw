import {test} from 'node:test';
import WebSocket from 'ws';
import {VoiceNotes} from './notes.mjs';
import {LiveTexts} from './live.mjs';
import {spawn,spawnSync} from 'node:child_process';
import {mkdtemp,rm,writeFile,readFile} from 'node:fs/promises';
import {tmpdir,homedir} from 'node:os';
import {join} from 'node:path';
import {randomUUID} from 'node:crypto';
import assert from 'node:assert/strict';
test('official CLI delivers live text updates and encrypted voice notes in both directions', {
 skip: process.env.SIMPLEX_CLI_TEST !== '1', timeout: 300000,
}, async () => {
process.umask(0o077);
const root=await mkdtemp(join(tmpdir(),'zeroclaw-simplex-proof-'));
const clients=[]; let notes;
const delay=ms=>new Promise(r=>setTimeout(r,ms));
async function start(name,port) {
 const proc=spawn(process.env.SIMPLEX_CLI_PATH ?? join(homedir(),'.local/share/zeroclaw-simplex/simplex-chat'),['-d',join(root,name),'-p',String(port),'--create-bot-display-name',name,'--mute'],{stdio:'ignore'});
 const client={proc,events:[],pending:new Map()};clients.push(client);
 for(let n=0;n<120;n++) {
  try {client.ws=await new Promise((resolve,reject)=>{const s=new WebSocket(`ws://127.0.0.1:${port}`);s.once('open',()=>resolve(s));s.once('error',reject);});break;} catch {await delay(500);}
 }
 assert.ok(client.ws,'CLI did not start');
 client.ws.on('message',data=>{const value=JSON.parse(data), p=client.pending.get(value.corrId);
  if(p){client.pending.delete(value.corrId);clearTimeout(p.timer);p.resolve(value.resp);}
  else {client.events.push(value.resp);client.onEvent?.(value.resp).catch(()=>{client.error=true;});}
 });
 client.rpc=cmd=>new Promise((resolve,reject)=>{const corrId=randomUUID();const timer=setTimeout(()=>{client.pending.delete(corrId);reject(Error('rpc_timeout'));},30000);client.pending.set(corrId,{resolve,reject,timer});client.ws.send(JSON.stringify({corrId,cmd}));});
 return client;
}
async function until(predicate,label) {for(let n=0;n<240;n++){const result=await predicate();if(result)return result;await delay(500);}throw Error(label);}
try {
 const a=await start('VoiceFixtureA',55231), b=await start('VoiceFixtureB',55232);
 const inv=await a.rpc('/_connect 1');
 assert.equal(inv.type,'invitation');
 const connected=await b.rpc('/_connect 1 '+inv.connLinkInvitation.connFullLink);
 assert.ok(!['chatCmdError','chatError'].includes(connected.type),'connect failed');
 const ca=await until(()=>a.events.find(e=>e.type==='contactConnected'),'first_contact_timeout');
 const cb=await until(()=>b.events.find(e=>e.type==='contactConnected'),'second_contact_timeout');
 const aid=String(ca.contact.contactId),bid=String(cb.contact.contactId);
 // Verify the real receiving core, including parsed formatting and live state.
 const live=new LiveTexts(async cmd=>{
  const response=await a.rpc(cmd);
  assert.ok(!['chatCmdError','chatError'].includes(response.type),'live command rejected: '+JSON.stringify(response));
  return response;
 });
 await live.start('fixture-live',aid,'Processing…');
 const preview=await until(()=>b.events.flatMap(e=>e.type==='newChatItems'?e.chatItems??[]:[])
  .find(i=>i.chatItem?.content?.msgContent?.text==='Processing…'),'live_placeholder_timeout');
 const physicalId=preview.chatItem.meta.itemId;
 assert.equal(preview.chatItem.meta.itemLive,true);
 await live.update('fixture-live',aid,'**Streaming** response');
 const partial=await until(()=>b.events.filter(e=>e.type==='chatItemUpdated')
  .map(e=>e.chatItem?.chatItem ?? e.chatItem).find(i=>i?.meta?.itemId===physicalId && i.content?.msgContent?.text==='*Streaming* response'),'live_update_timeout');
 assert.equal(partial.meta.itemLive,true);
 assert.ok(partial.formattedText?.some(s=>s.format?.type==='bold' && s.text==='Streaming'));
 await live.finish('fixture-live',aid,'**Streaming** response finished.');
 const final=await until(()=>b.events.filter(e=>e.type==='chatItemUpdated')
  .map(e=>e.chatItem?.chatItem ?? e.chatItem).find(i=>i?.meta?.itemId===physicalId && i.content?.msgContent?.text==='*Streaming* response finished.'),'live_finalize_timeout');
 assert.notEqual(final.meta.itemLive,true);
 assert.equal(b.events.flatMap(e=>e.type==='newChatItems'?e.chatItems??[]:[])
  .filter(i=>i.chatItem?.meta?.itemId===physicalId).length,1,'edits must retain the physical message');
 await live.start('fixture-cancel',aid,'Thinking…');
 await live.cancel('fixture-cancel',aid,'[Interrupted]');
 const result=spawnSync('ffmpeg',['-v','error','-f','lavfi','-i','anullsrc=r=16000:cl=mono','-t','0.4','-c:a','aac','-movflags','frag_keyframe+empty_moov','-f','mp4','pipe:1']);assert.equal(result.status,0);
 const events=[];notes=new VoiceNotes(async cmd=>{const response=await a.rpc(cmd);assert.ok(!['chatCmdError','chatError'].includes(response.type),'send rejected: '+response.type);return response;},async event=>events.push(event));
 a.onEvent=event=>notes.event(event);
 b.events=[];
 await notes.send(aid,'**Fixture** text plus voice.',result.stdout.toString('base64'));
 const incoming=await until(()=>b.events.find(e=>e.type==='newChatItems' && e.chatItems?.some(i=>i.chatItem?.content?.msgContent?.type==='voice')),'voice_invitation_timeout');
 const wrapper=incoming.chatItems.find(i=>i.chatItem.content.msgContent.type==='voice');
 const receivedText=await until(()=>b.events.flatMap(e=>e.type==='newChatItems' ? e.chatItems ?? [] : []).find(i=>i.chatItem?.content?.msgContent?.text==='*Fixture* text plus voice.'),'formatted_text_timeout');
 assert.equal(receivedText.chatItem.content.msgContent.text,'*Fixture* text plus voice.');
 assert.ok(receivedText.chatItem.formattedText?.some(span=>span.format?.type==='bold' && span.text==='Fixture'),
   'receiving SimpleX core must recognize a bold span, not display double asterisks');
 const path=join(root,'received.m4a');
 const accepted=await b.rpc(`/freceive ${wrapper.chatItem.file.fileId} encrypt=off ${path}`);
 assert.ok(!['chatCmdError','chatError'].includes(accepted.type),'receive rejected');
 await until(()=>b.events.find(e=>e.type==='rcvFileComplete'),'download_timeout');
 const receivedAudio=await readFile(path);
 const headers=spawnSync('ffprobe',['-v','error','-show_entries','format=duration:stream=codec_name,profile','-of','json',path]);
 assert.equal(headers.status,0);
 const metadata=JSON.parse(headers.stdout);assert.ok(Number(metadata.format.duration)>0);
 assert.equal(metadata.streams[0].codec_name,'aac');assert.equal(metadata.streams[0].profile,'LC');
 // Verify the receiver got a finalized movie, not merely a decodable fragment.
 let offset=0, finalized=false;
 while(offset+8<=receivedAudio.length) {
  const size=receivedAudio.readUInt32BE(offset),type=receivedAudio.toString('ascii',offset+4,offset+8);
  assert.ok(size>=8);assert.notEqual(type,'moof');
  if(type==='moov') {
   let child=offset+8;
   while(child+8<=offset+size) {
    const childSize=receivedAudio.readUInt32BE(child);
    if(receivedAudio.toString('ascii',child+4,child+8)==='mvhd') finalized=receivedAudio.readUInt32BE(child+24)>0;
    assert.ok(childSize>=8);child+=childSize;
   }
  }
  offset+=size;
 }
 assert.ok(finalized,'received file must have a nonzero movie-header duration');
 await until(()=>notes.sending.size===0,'upload_cleanup_timeout');
 const voiceSource=join(root,'input.m4a');await writeFile(voiceSource,result.stdout);
 const sent=await b.rpc(`/_send @${bid} json ${JSON.stringify([{fileSource:{filePath:voiceSource},msgContent:{type:'voice',text:'',duration:1}}])}`);
 assert.ok(!['chatCmdError','chatError'].includes(sent.type),'reverse send rejected');
 const inbound=await until(()=>a.events.find(e=>e.type==='newChatItems' && e.chatItems?.some(i=>i.chatItem?.content?.msgContent?.type==='voice')),'reverse_invitation_timeout');
 await notes.invitation(inbound.chatItems.find(i=>i.chatItem.content.msgContent.type==='voice'));
 const vi=events.find(e=>e.type==='voice_invitation');assert.ok(vi);
 await notes.receive({...vi,message_id:vi.id,max_duration:600});
 const complete=await until(()=>events.find(e=>e.type==='voice_note'),'reverse_download_timeout');
 assert.equal(complete.contact,aid);assert.equal(complete.id,vi.id);
 assert.deepEqual(Buffer.from(complete.audio,'base64'),result.stdout);
 assert.ok(!a.error);
 assert.equal(notes.receiving.size,0);
} finally {
 await notes?.close();
 for(const c of clients){c.ws?.terminate();for(const p of c.pending.values())clearTimeout(p.timer);c.proc.kill('SIGTERM');}
 await delay(1000);for(const c of clients)c.proc.kill('SIGKILL');await rm(root,{recursive:true,force:true});
}

});
