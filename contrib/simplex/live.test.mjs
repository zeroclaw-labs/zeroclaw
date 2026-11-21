import {test} from 'node:test';
import assert from 'node:assert/strict';
import {LiveTexts} from './live.mjs';
import {simplexTextMessages} from './format.mjs';

function fixture() {
  const commands=[], items=new Map(); let sequence=0;
  const rpc=async cmd=>{
    commands.push(cmd);
    if(cmd.startsWith('/_send ')) {
      const message=JSON.parse(cmd.split(' json ')[1])[0];
      const id=++sequence;items.set(id,{text:message.msgContent.text,live:cmd.includes(' live=on')});
      return {type:'newChatItems',chatItems:[{chatInfo:{type:'direct',contact:{contactId:17}},
        chatItem:{meta:{itemId:id},chatDir:{type:'directSnd'}}}]};
    }
    if(cmd.startsWith('/_update ')) {
      const id=Number(cmd.split(' ')[3]);
      assert(items.has(id));
      items.set(id,{text:JSON.parse(cmd.split(' json ')[1]).msgContent.text,live:cmd.includes(' live=on')});
      return {type:'chatItemUpdated'};
    }
    if(cmd.startsWith('/_delete ')) {items.delete(Number(cmd.split(' ')[3]));return {type:'chatItemsDeleted'};}
    throw Error('unexpected fixture command');
  };
  return {rpc,commands,items,live:new LiveTexts(rpc)};
}
test('placeholder, streamed Markdown and final answer reuse one physical live item',async()=>{
  const {live,items,commands}=fixture();
  await live.start('draft','17','Processing…');
  await live.update('draft','17','Thinking…',true);
  await live.update('draft','17','**The answer**');
  await live.update('draft','17','Working…',true);
  assert.equal(items.get(1).text,'*The answer*');
  assert.equal(items.get(1).live,true);
  await live.finish('draft','17','**The answer** is ready.');
  assert.equal(items.size,1);
  assert.deepEqual(items.get(1),{text:'*The answer* is ready.',live:false});
  assert.equal(commands.filter(c=>c.startsWith('/_send ')).length,1);
  assert.equal(live.drafts.size,0);
  await assert.rejects(live.finish('draft','17','duplicate'));
  assert.equal(items.size,1);
});
test('completed tool narration is committed once and a later phase owns a new bubble',async()=>{
  const {live,items,commands}=fixture();
  await live.start('draft','17','Processing…');
  await live.update('draft','17','Checking now.');
  await live.flush('draft','17','Checking now.','receipt-one');
  await live.flush('draft','17','Checking now.','receipt-one');
  await live.update('draft','17','Working…',true);
  await live.update('draft','17','The final answer');
  await live.finish('draft','17','The final answer.');
  assert.deepEqual([...items.values()],[{text:'Checking now.',live:false},{text:'The final answer.',live:false}]);
  assert.equal(commands.filter(c=>c.startsWith('/_send ')).length,2);
});
test('cancellation removes empty placeholders and preserves interrupted partial text',async()=>{
  for(const partial of ['', 'A partial response']) {
    const {live,items}=fixture();await live.start('draft','17','Processing…');
    if(partial)await live.update('draft','17',partial);
    await live.cancel('draft','17','[Interrupted]');
    assert.equal(items.size,partial?1:0);
    if(partial)assert.deepEqual(items.get(1),{text:partial+'\n\n[Interrupted]',live:false});
  }
});
test('a rewrite removes only excess owned chunks and Unicode stays within the wire limit',async()=>{
  const {live,items}=fixture();await live.start('draft','17','Processing…');
  const text='**'+'😀'.repeat(10000)+'**';
  const messages=simplexTextMessages(text);
  assert(messages.length>1);
  assert(messages.every(m=>Buffer.byteLength(JSON.stringify(m))<=14500));
  await live.update('draft','17',text);
  assert.equal(items.size,messages.length);
  await live.finish('draft','17','Short final answer');
  assert.deepEqual([...items.values()],[{text:'Short final answer',live:false}]);
});
test('scope checks, creation ACK loss and uncertain final edits do not resend answers',async()=>{
  const f=fixture();await f.live.start('draft','17','Processing…');
  const before=f.commands.length;
  await assert.rejects(f.live.update('draft','18','private'));
  assert.equal(f.commands.length,before);
  let calls=0;
  const lost=new LiveTexts(async()=>{calls++;return {type:'cmdOk'};});
  await assert.rejects(lost.start('new','17','Processing…'),e=>e.noResend===true);
  assert.equal(calls,1);
  assert.equal(lost.drafts.size,0);
  f.live.rpc=async()=>{calls++;throw Error('lost acknowledgement');};
  const start=calls;
  await assert.rejects(f.live.finish('draft','17','Final answer'),e=>e.noResend===true);
  assert.equal(calls-start,2,'retry only the same item edit, never create a duplicate');
});
test('an update in flight settles before finalization and cannot reopen the finalized item',async()=>{
  const f=fixture();await f.live.start('draft','17','Processing…');
  let release;const gate=new Promise(resolve=>{release=resolve;});
  const normal=f.live.rpc;let first=true;
  f.live.rpc=async cmd=>{if(first){first=false;await gate;}return normal(cmd);};
  const update=f.live.update('draft','17','Partial');
  const finish=f.live.finish('draft','17','Complete');
  await assert.rejects(f.live.update('draft','17','late'));
  release();await update;await finish;
  assert.deepEqual(f.items.get(1),{text:'Complete',live:false});
});
test('reconnect discovery only emits sent live item metadata; recovery requires native authorization',async()=>{
  const events=[], commands=[];let response;
  const now=Date.now();
  const rpc=async cmd=>{
    commands.push(cmd);
    if(cmd==='/u')return {type:'activeUser',user:{userId:1}};
    if(cmd.startsWith('/_get chats'))return {chats:[{chatInfo:{type:'direct',contact:{contactId:17}}}]};
    if(cmd.startsWith('/_get chat'))return {chat:{chatItems:[
      {meta:{itemId:42,itemLive:true,createdAt:new Date(now-2*3600000).toISOString()},chatDir:{type:'directSnd'},content:{type:'sndMsgContent',msgContent:{type:'text',text:'*Bold* _italic_'}}},
      {meta:{itemId:43,itemLive:true,createdAt:new Date(now).toISOString()},chatDir:{type:'directRcv'},content:{type:'rcvMsgContent',msgContent:{type:'text',text:'Untrusted'}}},
    ]}};
    response=cmd;return {type:'chatItemUpdated'};
  };
  const live=new LiveTexts(rpc);await live.discover(async e=>events.push(e),now);
  assert.deepEqual(events,[{type:'live_orphan',contact:'17',item_id:42}]);
  assert(commands.every(cmd=>!cmd.startsWith('/_update')));
  await assert.rejects(live.recover('18',42,'[Interrupted]',[]));
  await live.recover('17',42,'[Interrupted]',[]);
  assert(response.startsWith('/_update item @17 42 json '));
  assert.equal(JSON.parse(response.split(' json ')[1]).msgContent.text,'*Bold* _italic_\n\n[Interrupted]');
});
