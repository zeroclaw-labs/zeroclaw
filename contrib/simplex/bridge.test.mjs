import {test} from 'node:test';
import assert from 'node:assert/strict';
import {WebSocketServer} from 'ws';
import {spawn} from 'node:child_process';
import {createInterface} from 'node:readline';
import {once} from 'node:events';
import {fileURLToPath} from 'node:url';

test('bridge carries private text, excludes sent/group events, and rejects unencrypted calls', async () => {
  const server = new WebSocketServer({host: '127.0.0.1', port: 0}); await once(server, 'listening');
  const child = spawn(process.execPath, [fileURLToPath(new URL('./bridge.mjs', import.meta.url))], {stdio: ['pipe', 'pipe', 'ignore']});
  const lines = createInterface({input: child.stdout});
  const queue = []; let notify;
  lines.on('line', line => {queue.push(JSON.parse(line)); notify?.();});
  async function next() {
    const timeout = Date.now() + 3000;
    while (!queue.length && Date.now() < timeout) await new Promise(resolve => {notify = resolve; setTimeout(resolve, 50);});
    assert.ok(queue.length, 'bridge response timed out'); return queue.shift();
  }
  const send = value => child.stdin.write(JSON.stringify(value) + '\n');
  try {
    send({type: 'init', id: 'init', websocket_url: `ws://127.0.0.1:${server.address().port}`});
    const [socket] = await once(server, 'connection');
    const commands = [];
    socket.on('message', data => {const message = JSON.parse(data); commands.push(message.cmd);
      socket.send(JSON.stringify({corrId: message.corrId, resp: {type: 'cmdOk'}}));});
    assert.deepEqual(await next(), {type: 'result', id: 'init', ok: true});
    send({type: 'send', id: 'send', contact: '17', text: '**A private reply** with *emphasis* and `**literal**`'});
    assert.equal((await next()).ok, true);
    const outbound = commands.find(cmd => cmd.startsWith('/_send @17 json '));
    assert.ok(outbound);
    assert.equal(JSON.parse(outbound.slice('/_send @17 json '.length))[0].msgContent.text,
      '*A private reply* with _emphasis_ and `**literal**`');
    const item = {chatInfo: {type: 'direct', contact: {contactId: 17}}, chatItem: {meta: {itemId: 42}, chatDir: {type: 'directRcv'}, content: {type: 'rcvMsgContent', msgContent: {type: 'text', text: 'Hello'}}}};
    socket.send(JSON.stringify({resp: {type: 'newChatItems', chatItems: [item]}}));
    assert.deepEqual(await next(), {type: 'message', contact: '17', id: '42', text: 'Hello'});
    item.chatItem.chatDir.type = 'directSnd';
    socket.send(JSON.stringify({resp: {type: 'newChatItems', chatItems: [item]}}));
    socket.send(JSON.stringify({resp: {type: 'callInvitation', callInvitation: {contact: {contactId: 17}, callType: {media: 'audio', capabilities: {encryption: false}}}}}));
    const deadline = Date.now() + 3000;
    while (!commands.some(c => c === '/_call reject @17') && Date.now() < deadline) await new Promise(resolve => setTimeout(resolve, 20));
    assert.ok(commands.includes('/_call reject @17')); assert.equal(queue.length, 0);
  } finally {child.stdin.end(); child.kill('SIGTERM'); for (const socket of server.clients) socket.terminate(); await new Promise(resolve => server.close(resolve));}
});

test('stdio bridge streams and finalizes one live message, then recovers only authorized owned items', async () => {
  const server = new WebSocketServer({host:'127.0.0.1',port:0}); await once(server,'listening');
  const child = spawn(process.execPath,[fileURLToPath(new URL('./bridge.mjs',import.meta.url))],{stdio:['pipe','pipe','ignore']});
  const lines=createInterface({input:child.stdout}), frames=[], commands=[];
  lines.on('line',line=>frames.push(JSON.parse(line)));
  async function wait(predicate) {
    const deadline=Date.now()+4000;
    while(Date.now()<deadline) {const result=predicate();if(result)return result;await new Promise(r=>setTimeout(r,10));}
    assert.fail('live bridge boundary timed out: '+JSON.stringify({frames,commands}));
  }
  let sequence=100;
  const items=new Map();
  async function command(type,extra={}) {
    const id=String(++sequence);
    child.stdin.write(JSON.stringify({type,id,contact:'17',draft:'fixture',...extra})+'\n');
    return wait(()=>frames.find(f=>f.type==='result' && f.id===id));
  }
  try {
    const connected=once(server,'connection');
    const initialized=command('init',{websocket_url:`ws://127.0.0.1:${server.address().port}`});
    const [socket]=await connected;
    socket.on('message',data=>{
      const {corrId,cmd}=JSON.parse(data);commands.push(cmd);
      let resp;
      if(cmd==='/u')resp={type:'activeUser',user:{userId:1}};
      else if(cmd.startsWith('/_get chats'))resp={type:'apiChats',chats:[{chatInfo:{type:'direct',contact:{contactId:17}}}]};
      else if(cmd.startsWith('/_get chat'))resp={type:'apiChat',chat:{chatItems:[{
        meta:{itemId:42,itemLive:true,createdAt:new Date(Date.now()-2*3600000).toISOString()},chatDir:{type:'directSnd'},
        content:{type:'sndMsgContent',msgContent:{type:'text',text:'Earlier partial'}},
      }]}};
      else if(cmd.startsWith('/_send ')) {
        const message=JSON.parse(cmd.split(' json ')[1])[0], itemId=++sequence;
        items.set(itemId,{text:message.msgContent.text,live:cmd.includes(' live=on')});
        resp={type:'newChatItems',chatItems:[{chatInfo:{type:'direct',contact:{contactId:17}},chatItem:{meta:{itemId},chatDir:{type:'directSnd'}}}]};
      } else if(cmd.startsWith('/_update ')) {
        const itemId=Number(cmd.split(' ')[3]);
        items.set(itemId,{text:JSON.parse(cmd.split(' json ')[1]).msgContent.text,live:cmd.includes(' live=on')});
        resp={type:'chatItemUpdated'};
      } else if(cmd.startsWith('/_delete ')) {items.delete(Number(cmd.split(' ')[3]));resp={type:'chatItemsDeleted'};}
      else resp={type:'cmdOk'};
      socket.send(JSON.stringify({corrId,resp}));
    });
    assert.equal((await initialized).ok,true);
    await wait(()=>frames.find(f=>f.type==='live_orphan'));
    assert(!commands.some(c=>c.startsWith('/_update')),'discovery cannot edit before native authorization');
    assert.equal((await command('live_recover',{item_id:42,interrupted:'[Interrupted]',placeholders:[]})).ok,true);
    assert.deepEqual(items.get(42),{text:'Earlier partial\n\n[Interrupted]',live:false});
    assert.equal((await command('live_start',{text:'Processing…'})).ok,true);
    assert.equal((await command('live_status',{text:'Thinking…'})).ok,true);
    assert.equal((await command('live_update',{text:'**Hello**'})).ok,true);
    assert.equal((await command('live_status',{text:'Working…'})).ok,true);
    assert.equal((await command('live_finish',{text:'**Hello** there.'})).ok,true);
    const created=commands.filter(c=>c.startsWith('/_send '));
    assert.equal(created.length,1);
    assert.deepEqual([...items.values()].at(-1),{text:'*Hello* there.',live:false});
    assert.equal((await command('live_finish',{text:'duplicate'})).ok,false);
    assert.equal(commands.filter(c=>c.startsWith('/_send ')).length,1);
  } finally {
    child.stdin.end();child.kill('SIGTERM');lines.close();
    for(const socket of server.clients)socket.terminate();
    await new Promise(resolve=>server.close(resolve));
  }
});
