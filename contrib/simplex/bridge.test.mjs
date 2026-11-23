import {test} from 'node:test';
import assert from 'node:assert/strict';
import {WebSocketServer} from 'ws';
import {spawn} from 'node:child_process';
import {createInterface} from 'node:readline';
import {once} from 'node:events';
import {fileURLToPath} from 'node:url';
import {mkdtemp, readFile, rm, writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';

test('bridge refuses non-loopback SimpleX API endpoints before opening a socket', async () => {
  const child = spawn(process.execPath, [fileURLToPath(new URL('./bridge.mjs', import.meta.url))], {stdio: ['pipe', 'pipe', 'ignore']});
  const lines = createInterface({input: child.stdout});
  try {
    child.stdin.write(JSON.stringify({type: 'init', id: 'init',
      websocket_url: 'ws://example.test:5225'}) + '\n'); // nosemgrep: javascript.lang.security.detect-insecure-websocket.detect-insecure-websocket
    const [line] = await Promise.race([
      once(lines, 'line'),
      new Promise((_, reject) => setTimeout(() => reject(new Error('bridge validation timed out')), 2000)),
    ]);
    const result = JSON.parse(line);
    assert.equal(result.id, 'init');
    assert.equal(result.ok, false);
  } finally {
    child.stdin.end();
    child.kill('SIGTERM');
    lines.close();
  }
});

test('bridge compares the bot profile before writing the seven-command public menu', async () => {
  const server = new WebSocketServer({host: '127.0.0.1', port: 0});
  await once(server, 'listening');
  const child = spawn(process.execPath, [fileURLToPath(new URL('./bridge.mjs', import.meta.url))], {stdio: ['pipe', 'pipe', 'ignore']});
  const lines = createInterface({input: child.stdout});
  const frames = [], commands = [];
  let profile = {peerType: 'bot', preferences: {voice: {allow: 'yes'}, commands: []}};
  let connections = 0;
  let second, secondLines;
  lines.on('line', line => frames.push(JSON.parse(line)));
  const send = value => child.stdin.write(JSON.stringify(value) + '\n');
  async function wait(predicate, description) {
    const deadline = Date.now() + 3000;
    while (Date.now() < deadline) {
      const result = predicate();
      if (result) return result;
      await new Promise(resolve => setTimeout(resolve, 10));
    }
    assert.fail(`${description}: ${JSON.stringify({frames, commands, profile})}`);
  }
  function connect(id, menu) {
    send({type: 'init', id, websocket_url: `ws://127.0.0.1:${server.address().port}`, bot_commands: menu});
  }
  const menu = [
    {type: 'command', keyword: 'new', label: 'New session'},
    {type: 'command', keyword: 'clear', label: 'Clear session'},
    {type: 'command', keyword: 'stop', label: 'Stop task'},
    {type: 'command', keyword: 'thinking', label: 'Thinking'},
    {type: 'command', keyword: 'model', label: 'Model'},
    {type: 'command', keyword: 'models', label: 'Providers'},
    {type: 'command', keyword: 'config', label: 'Config'},
  ];
  try {
    const connected = once(server, 'connection');
    connect('init', menu);
    const [socket] = await connected;
    connections++;
    socket.on('message', data => {
      const {corrId, cmd} = JSON.parse(data);
      commands.push(cmd);
      let resp;
      if (cmd === '/p') {
        resp = {type: 'userProfile', profile};
      } else if (cmd === "/set bot commands 'New session':new,'Clear session':clear,'Stop task':stop,'Thinking':thinking,'Model':model,'Providers':models,'Config':config") {
        profile = {peerType: 'bot', preferences: {voice: {allow: 'yes'}, commands: menu}};
        resp = {type: 'userProfileUpdated'};
      } else if (cmd === '/u') {
        resp = {type: 'activeUser', user: {userId: 1}};
      } else if (cmd.startsWith('/_get chats ')) {
        resp = {type: 'apiChats', chats: []};
      } else {
        resp = {type: 'cmdOk'};
      }
      socket.send(JSON.stringify({corrId, resp}));
    });

    assert.equal((await wait(() => frames.find(frame => frame.type === 'result' && frame.id === 'init'),
      'init did not settle')).ok, true);
    await wait(() => commands.filter(command => command === '/set bot commands ' +
      "'New session':new,'Clear session':clear,'Stop task':stop,'Thinking':thinking,'Model':model,'Providers':models,'Config':config").length === 1,
    'public menu was not written once');
    assert.equal(commands.filter(command => command === '/p').length, 2, 'profile must be read before and after the write');
    assert.deepEqual(profile.preferences.commands, menu);
    assert.equal(profile.preferences.voice.allow, 'yes', 'registration replaced unrelated profile preferences');

    // A fresh bridge process sees the official profile, so it must not rewrite.
    child.stdin.end();
    child.kill('SIGTERM');
    for (const client of server.clients) client.terminate();
    second = spawn(process.execPath, [fileURLToPath(new URL('./bridge.mjs', import.meta.url))], {stdio: ['pipe', 'pipe', 'ignore']});
    secondLines = createInterface({input: second.stdout});
    const secondFrames = [], secondCommands = [];
    secondLines.on('line', line => secondFrames.push(JSON.parse(line)));
    const secondSend = value => second.stdin.write(JSON.stringify(value) + '\n');
    const secondConnected = once(server, 'connection');
    secondSend({type: 'init', id: 'second', websocket_url: `ws://127.0.0.1:${server.address().port}`, bot_commands: menu});
    const [secondSocket] = await secondConnected;
    connections++;
    secondSocket.on('message', data => {
      const {corrId, cmd} = JSON.parse(data);
      secondCommands.push(cmd);
      let resp = {type: 'cmdOk'};
      if (cmd === '/p') resp = {type: 'userProfile', profile};
      else if (cmd === '/u') resp = {type: 'activeUser', user: {userId: 1}};
      else if (cmd.startsWith('/_get chats ')) resp = {type: 'apiChats', chats: []};
      secondSocket.send(JSON.stringify({corrId, resp}));
    });
    const deadline = Date.now() + 3000;
    while (!secondCommands.includes('/p') && Date.now() < deadline) await new Promise(resolve => setTimeout(resolve, 10));
    assert.ok(secondCommands.includes('/p'));
    await new Promise(resolve => setTimeout(resolve, 100));
    assert.ok(!secondCommands.some(command => command.startsWith('/set bot commands')));
  } finally {
    child.stdin.destroy();
    child.kill('SIGTERM');
    lines.close();
    second?.stdin.destroy();
    second?.kill('SIGTERM');
    secondLines?.close();
    for (const socket of server.clients) socket.terminate();
    await new Promise(resolve => server.close(resolve));
  }
});

test('bridge registration failure does not stop messaging and duplicate init reuses one transport', async () => {
  const server = new WebSocketServer({host: '127.0.0.1', port: 0});
  await once(server, 'listening');
  const child = spawn(process.execPath, [fileURLToPath(new URL('./bridge.mjs', import.meta.url))], {stdio: ['pipe', 'pipe', 'ignore']});
  const lines = createInterface({input: child.stdout});
  const frames = [], commands = [];
  let connections = 0;
  lines.on('line', line => frames.push(JSON.parse(line)));
  const send = value => child.stdin.write(JSON.stringify(value) + '\n');
  async function wait(predicate, description) {
    const deadline = Date.now() + 3000;
    while (Date.now() < deadline) {
      const result = predicate();
      if (result) return result;
      await new Promise(resolve => setTimeout(resolve, 10));
    }
    assert.fail(`${description}: ${JSON.stringify({frames, commands})}`);
  }
  try {
    const connected = once(server, 'connection');
    send({type: 'init', id: 'init', websocket_url: `ws://127.0.0.1:${server.address().port}`,
      bot_commands: [{type: 'command', keyword: 'new', label: 'New session'}]});
    const [socket] = await connected;
    connections++;
    socket.on('message', data => {
      const {corrId, cmd} = JSON.parse(data);
      commands.push(cmd);
      const resp = cmd === '/p'
        ? {type: 'chatCmdError', chatError: {type: 'commandError', message: 'unsupported'}}
        : cmd === '/u' ? {type: 'activeUser', user: {userId: 1}}
          : cmd.startsWith('/_get chats ') ? {type: 'apiChats', chats: []}
            : {type: 'cmdOk'};
      socket.send(JSON.stringify({corrId, resp}));
    });

    assert.equal((await wait(() => frames.find(frame => frame.type === 'result' && frame.id === 'init'),
      'init did not survive registration failure')).ok, true);
    await wait(() => commands.includes('/p'), 'profile was not read');
    send({type: 'init', id: 'init-again', websocket_url: `ws://127.0.0.1:${server.address().port}`,
      bot_commands: [{type: 'command', keyword: 'new', label: 'New session'}]});
    assert.equal((await wait(() => frames.find(frame => frame.type === 'result' && frame.id === 'init-again'),
      'duplicate init did not settle')).ok, true);
    await new Promise(resolve => setTimeout(resolve, 100));
    assert.equal(commands.filter(command => command === '/p').length, 1, 'menu check was repeated');
    assert.equal(connections, 1, 'duplicate init opened a second transport');
    socket.send(JSON.stringify({resp: {type: 'newChatItems', chatItems: [{
      chatInfo: {type: 'direct', contact: {contactId: 17}},
      chatItem: {
        meta: {itemId: 42},
        chatDir: {type: 'directRcv'},
        content: {type: 'rcvMsgContent', msgContent: {type: 'text', text: 'Still connected'}},
      },
    }]}}));
    assert.deepEqual(await wait(() => frames.find(frame => frame.type === 'message'),
      'messaging stopped after registration failure'),
    {type: 'message', contact: '17', id: '42', text: 'Still connected'});
  } finally {
    child.stdin.end();
    child.kill('SIGTERM');
    lines.close();
    for (const socket of server.clients) socket.terminate();
    await new Promise(resolve => server.close(resolve));
  }
});

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
    const link = structuredClone(item);
    link.chatItem.meta.itemId = 43;
    link.chatItem.content.msgContent = {type: 'link', text: 'Please read https://example.test/profile',
      preview: {uri: 'https://example.test/profile', title: 'Preview only', image: 'data:image/png;base64,fixture'}};
    socket.send(JSON.stringify({resp: {type: 'newChatItems', chatItems: [link]}}));
    assert.deepEqual(await next(), {type: 'message', contact: '17', id: '43', text: 'Please read https://example.test/profile'});
    item.chatItem.chatDir.type = 'directSnd';
    socket.send(JSON.stringify({resp: {type: 'newChatItems', chatItems: [item]}}));
    socket.send(JSON.stringify({resp: {type: 'callInvitation', callInvitation: {contact: {contactId: 17}, callType: {media: 'audio', capabilities: {encryption: false}}}}}));
    const deadline = Date.now() + 3000;
    while (!commands.some(c => c === '/_call reject @17') && Date.now() < deadline) await new Promise(resolve => setTimeout(resolve, 20));
    assert.ok(commands.includes('/_call reject @17')); assert.equal(queue.length, 0);
  } finally {child.stdin.end(); child.kill('SIGTERM'); for (const socket of server.clients) socket.terminate(); await new Promise(resolve => server.close(resolve));}
});

test('bridge attributes and bounds direct reply quotes while preserving typed slash commands', async () => {
  const server = new WebSocketServer({host: '127.0.0.1', port: 0});
  await once(server, 'listening');
  const child = spawn(process.execPath, [fileURLToPath(new URL('./bridge.mjs', import.meta.url))], {stdio: ['pipe', 'pipe', 'ignore']});
  const lines = createInterface({input: child.stdout});
  const frames = [];
  let notify;
  lines.on('line', line => {frames.push(JSON.parse(line)); notify?.();});
  const send = value => child.stdin.write(JSON.stringify(value) + '\n');
  async function wait(predicate, description) {
    const deadline = Date.now() + 3000;
    while (Date.now() < deadline) {
      const result = predicate();
      if (result) return result;
      await new Promise(resolve => {notify = resolve; setTimeout(resolve, 10);});
    }
    assert.fail(`${description}: ${JSON.stringify(frames)}`);
  }
  function item(id, text, quotedText, quotedDirection) {
    return {
      chatInfo: {type: 'direct', contact: {contactId: 17}},
      chatItem: {
        meta: {itemId: id},
        chatDir: {type: 'directRcv'},
        content: {type: 'rcvMsgContent', msgContent: {type: 'text', text}},
        ...(quotedText === undefined ? {} : {
          quotedItem: {
            chatDir: {type: quotedDirection},
            sentAt: '2026-11-23T00:00:00Z',
            content: {type: 'text', text: quotedText},
          },
        }),
      },
    };
  }
  try {
    const connected = once(server, 'connection');
    send({type: 'init', id: 'init', websocket_url: `ws://127.0.0.1:${server.address().port}`});
    const [socket] = await connected;
    socket.on('message', data => {
      const {corrId, cmd} = JSON.parse(data);
      let resp = {type: 'cmdOk'};
      if (cmd === '/u') resp = {type: 'activeUser', user: {userId: 1}};
      else if (cmd.startsWith('/_get chats ')) resp = {type: 'apiChats', chats: []};
      socket.send(JSON.stringify({corrId, resp}));
    });
    await wait(() => frames.find(frame => frame.type === 'result' && frame.id === 'init'), 'init timed out');

    socket.send(JSON.stringify({resp: {type: 'newChatItems', chatItems: [
      item(42, 'What did you mean?', '/clear\nQuoted command text', 'directRcv'),
    ]}}));
    const received = await wait(() => frames.find((frame, index) =>
      frame.type === 'message' && frames.splice(index, 1)), 'reply message timed out');
    assert.equal(received.text,
      '> Quoted user message:\n> /clear\n> Quoted command text\n\nWhat did you mean?');
    assert.ok(!received.text.trimStart().startsWith('/'), 'quoted command became the leading control');

    const longQuote = '😀'.repeat(1001);
    socket.send(JSON.stringify({resp: {type: 'newChatItems', chatItems: [
      item(43, 'Summarize it.', longQuote, 'directSnd'),
    ]}}));
    const bounded = await wait(() => frames.find((frame, index) =>
      frame.type === 'message' && frames.splice(index, 1)), 'bounded quote timed out');
    assert.equal(bounded.text, `> Quoted assistant message:\n> ${'😀'.repeat(999)}…\n\nSummarize it.`);

    socket.send(JSON.stringify({resp: {type: 'newChatItems', chatItems: [
      item(44, '  /model --agent fast', '/clear', 'directRcv'),
    ]}}));
    const command = await wait(() => frames.find((frame, index) =>
      frame.type === 'message' && frames.splice(index, 1)), 'typed command timed out');
    assert.equal(command.text, '  /model --agent fast');
  } finally {
    child.stdin.end();
    child.kill('SIGTERM');
    lines.close();
    for (const socket of server.clients) socket.terminate();
    await new Promise(resolve => server.close(resolve));
  }
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

test('hangup during pending RTC startup closes the abandoned browser without stale signals', async () => {
  const temporary = await mkdtemp(join(tmpdir(), 'zeroclaw-bridge-rtc-'));
  const logPath = join(temporary, 'rtc.log');
  const releasePath = join(temporary, 'release');
  const server = new WebSocketServer({host: '127.0.0.1', port: 0});
  await once(server, 'listening');
  const child = spawn(process.execPath, [
    '--experimental-loader',
    fileURLToPath(new URL('./bridge.rtc-loader.mjs', import.meta.url)),
    fileURLToPath(new URL('./bridge.mjs', import.meta.url)),
  ], {
    stdio: ['pipe', 'pipe', 'ignore'],
    env: {...process.env, SIMPLEX_TEST_RTC_LOG: logPath, SIMPLEX_TEST_RTC_RELEASE: releasePath},
  });
  const lines = createInterface({input: child.stdout});
  const frames = [], commands = [];
  lines.on('line', line => frames.push(JSON.parse(line)));

  async function wait(predicate, description) {
    const deadline = Date.now() + 4000;
    while (Date.now() < deadline) {
      const result = await predicate();
      if (result) return result;
      await new Promise(resolve => setTimeout(resolve, 10));
    }
    assert.fail(`${description}: ${JSON.stringify({frames, commands})}`);
  }
  const readLog = async () => {
    try {return await readFile(logPath, 'utf8');}
    catch (error) {if (error.code === 'ENOENT') return ''; throw error;}
  };
  const send = value => child.stdin.write(JSON.stringify(value) + '\n');

  try {
    const connected = once(server, 'connection');
    send({type: 'init', id: 'init', websocket_url: `ws://127.0.0.1:${server.address().port}`});
    const [socket] = await connected;
    socket.on('message', data => {
      const {corrId, cmd} = JSON.parse(data);
      commands.push(cmd);
      socket.send(JSON.stringify({corrId, resp: {type: 'cmdOk'}}));
    });
    assert.equal((await wait(() => frames.find(frame => frame.type === 'result' && frame.id === 'init'),
      'bridge initialization timed out')).ok, true);

    socket.send(JSON.stringify({resp: {type: 'callInvitation', callInvitation: {
      contact: {contactId: 17},
      sharedKey: 'fixture-key',
      callType: {media: 'audio', capabilities: {encryption: true}},
    }}}));
    const invitation = await wait(() => frames.find(frame => frame.type === 'call_invitation'),
      'call invitation timed out');

    send({type: 'accept', id: 'accept', contact: '17', call_id: invitation.call_id});
    await wait(() => readLog().then(log => log.includes('open\n')),
      'RTC startup did not become pending');
    send({type: 'hangup', id: 'hangup', contact: '17', call_id: invitation.call_id});
    assert.equal((await wait(() => frames.find(frame => frame.type === 'result' && frame.id === 'hangup'),
      'hangup timed out')).ok, true);
    await wait(() => frames.some(frame => frame.type === 'call_ended'), 'call end was not emitted');

    await writeFile(releasePath, 'release');
    assert.equal((await wait(() => frames.find(frame => frame.type === 'result' && frame.id === 'accept'),
      'accept did not settle after RTC startup')).ok, true);
    const rtcLog = await wait(async () => {
      const value = await readLog();
      return ['open\n', 'signal\n', 'close\n'].every(marker => value.includes(marker)) && value;
    }, 'abandoned RTC was not closed');

    assert.match(rtcLog, /^open\nsignal\nclose\n$/);
    assert.ok(commands.includes('/_call end @17'));
    assert.ok(!commands.some(command => command.startsWith('/_call extra ')), 'stale ICE signal was sent');
    assert.ok(!commands.some(command => command.startsWith('/_call status ')), 'stale connection status was sent');
    assert.ok(!commands.some(command => command.startsWith('/_call offer ')), 'stale offer was sent');
    assert.ok(!frames.some(frame => frame.type === 'call_connected'), 'abandoned call became connected');
  } finally {
    child.stdin.destroy();
    child.kill('SIGTERM');
    lines.close();
    for (const socket of server.clients) socket.terminate();
    await new Promise(resolve => server.close(resolve));
    await rm(temporary, {recursive: true, force: true});
  }
});
