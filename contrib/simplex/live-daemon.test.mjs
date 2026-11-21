import {test} from 'node:test';
import assert from 'node:assert/strict';
import {WebSocketServer} from 'ws';
import {once} from 'node:events';
import {createServer} from 'node:http';
import {mkdtemp,writeFile,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {fileURLToPath} from 'node:url';
import {spawn} from 'node:child_process';
const pause=ms=>new Promise(r=>setTimeout(r,ms));
const listen=async server=>{server.listen(0,'127.0.0.1');await once(server,'listening');return server.address().port;};

test('daemon receives SimpleX input and delivers live tool narration and final text through the real bridge',{
  skip:process.env.SIMPLEX_DAEMON_TEST!=='1',timeout:90000,
},async()=>{
  const root=await mkdtemp(join(tmpdir(),'zeroclaw-simplex-live-daemon-'));
  const requests=[],commands=[],observed=[],items=new Map();let sequence=100,socket,daemon;
  const vendor=new WebSocketServer({host:'127.0.0.1',port:0});await once(vendor,'listening');
  vendor.on('connection',ws=>{socket=ws;ws.on('message',data=>{
    const {corrId,cmd}=JSON.parse(data);commands.push(cmd);let resp={type:'cmdOk'};
    if(cmd==='/u')resp={type:'activeUser',user:{userId:1}};
    else if(cmd.startsWith('/_get chats'))resp={type:'apiChats',chats:[]};
    else if(cmd.startsWith('/_send ')){
      const message=JSON.parse(cmd.split(' json ')[1])[0],itemId=++sequence;
      const state={id:itemId,text:message.msgContent.text,live:cmd.includes(' live=on')};
      items.set(itemId,state);observed.push({...state});
      resp={type:'newChatItems',chatItems:[{chatInfo:{type:'direct',contact:{contactId:17}},
        chatItem:{meta:{itemId},chatDir:{type:'directSnd'},content:{type:'sndMsgContent',msgContent:message.msgContent}}}]};
    }else if(cmd.startsWith('/_update ')){
      const itemId=Number(cmd.split(' ')[3]);assert(items.has(itemId),'update must own a created message');
      const state={id:itemId,text:JSON.parse(cmd.split(' json ')[1]).msgContent.text,live:cmd.includes(' live=on')};
      items.set(itemId,state);observed.push({...state});resp={type:'chatItemUpdated'};
    }else if(cmd.startsWith('/_delete ')){items.delete(Number(cmd.split(' ')[3]));resp={type:'chatItemsDeleted'};}
    ws.send(JSON.stringify({corrId,resp}));
  });});
  let modelError;
  const model=createServer(async(req,res)=>{try{
    let raw='';for await(const part of req)raw+=part;
    if(req.method!=='POST'){res.end('{}');return;}
    const body=JSON.parse(raw);requests.push(body);
    const messages=body.messages??[],serialized=JSON.stringify(messages);
    const followup=serialized.includes('fixture followup');
    if(followup){assert(serialized.includes('The **answer** is ready.'));}
    assert(!serialized.includes('Processing…')&&!serialized.includes('Thinking…')&&!serialized.includes('Working…'),
      'transport status must not enter model history');
    const toolPhase=!followup&&!messages.some(m=>m.role==='tool');
    const observedBeforePrefill=observed.length;
    await pause(1300);
    if(toolPhase){
      assert(!observed.some(i=>i.text==='Thinking…'),
        'waiting for the first provider output must remain Processing, not Thinking');
    }else{
      assert(!observed.slice(observedBeforePrefill).some(i=>i.text==='Thinking…'),
        'a request without reasoning must not introduce a Thinking status');
    }
    assert(body.stream,'SimpleX must request provider streaming');
    res.writeHead(200,{'Content-Type':'text/event-stream'});
    const emit=(delta,finish_reason=null)=>res.write('data: '+JSON.stringify({choices:[{index:0,delta,finish_reason}]})+'\n\n');
    emit({role:'assistant'});
    if(toolPhase){
      emit({reasoning_content:'PRIVATE_REASONING_FIXTURE'});
      await pause(750);
    }
    const fragments=followup?['Continuity ','confirmed.']:toolPhase?['Checking ','now.']:['The **answer**',' is ready.'];
    for(const content of fragments){emit({content});await pause(650);}
    if(toolPhase){emit({tool_calls:[{index:0,id:'fixture-call',type:'function',function:{name:'memory_recall',arguments:'{"query":"fixture"}'}}]});emit({},'tool_calls');}
    else emit({},'stop');
    res.end('data: [DONE]\n\n');
  }catch(error){modelError=error;res.destroy();}});
  const modelPort=await listen(model),reservation=createServer(),gatewayPort=await listen(reservation);
  await new Promise(r=>reservation.close(r));
  await writeFile(join(root,'config.toml'),`schema_version = 4
[gateway]
host = "127.0.0.1"
port = ${gatewayPort}
require_pairing = true
session_persistence = true
check_updates = false
[channels]
session_persistence = true
session_backend = "sqlite"
show_tool_calls = false
[channels.simplex.fixture]
enabled = true
websocket_url = "ws://127.0.0.1:${vendor.address().port}"
bridge_command = [${JSON.stringify(process.execPath)}, ${JSON.stringify(fileURLToPath(new URL('./bridge.mjs',import.meta.url)))}]
[peer_groups.fixture]
channel = "simplex.fixture"
external_peers = ["17"]
[memory]
backend = "none"
auto_save = false
[providers.models.ollama.fixture]
model = "fixture-model"
uri = "http://127.0.0.1:${modelPort}"
timeout_secs = 30
context_window = 128000
[risk_profiles.fixture]
level = "supervised"
[runtime_profiles.fixture]
max_context_tokens = 64000
max_history_messages = 200
[agents.fixture]
model_provider = "ollama.fixture"
risk_profile = "fixture"
runtime_profile = "fixture"
channels = ["simplex.fixture"]
`,{mode:0o600});
  let logs='';
  async function until(predicate,label){const deadline=Date.now()+40000;while(Date.now()<deadline){if(modelError)throw modelError;if(daemon?.exitCode!==null&&daemon?.exitCode!==undefined)assert.fail('fixture daemon exited: '+logs);const value=predicate();if(value)return value;await pause(40);}assert.fail(label+'; observed='+JSON.stringify(observed)+'; requests='+requests.length+'; logs='+logs.slice(-4500));}
  const input=text=>socket.send(JSON.stringify({resp:{type:'newChatItems',chatItems:[{
    chatInfo:{type:'direct',contact:{contactId:17}},chatItem:{meta:{itemId:++sequence},chatDir:{type:'directRcv'},content:{type:'rcvMsgContent',msgContent:{type:'text',text}}},
  }]}}));
  try{
    daemon=spawn(process.env.ZEROCLAW_TEST_BINARY??fileURLToPath(new URL('../../target/release/zeroclaw',import.meta.url)),['--config-dir',root,'daemon'],{
      env:{...process.env,ZEROCLAW_CONFIG_DIR:root},stdio:['ignore','pipe','pipe'],
    });
    for(const output of [daemon.stdout,daemon.stderr])output.on('data',data=>{logs=(logs+data).slice(-15000);});
    await until(()=>socket&&commands.includes('/u'),'bridge ready');
    input('fixture first user');
    await until(()=>observed.some(i=>i.text==='Processing…'&&i.live),'placeholder before model');
    await until(()=>observed.some(i=>i.text==='Thinking…'&&i.live),'thinking status');
    await until(()=>[...items.values()].some(i=>i.text==='The *answer* is ready.'&&!i.live),'final live answer');
    const finished=[...items.values()];
    assert.deepEqual(finished.map(i=>i.text),['Checking now.','The *answer* is ready.']);
    assert(finished.every(i=>!i.live));
    assert(observed.some(i=>i.text==='The *answer*'&&i.live),'partial answer must reach transport before final: '+JSON.stringify(observed));
    assert(!observed.some(i=>i.text.includes('PRIVATE_REASONING_FIXTURE')||i.text.includes('memory_recall')),'reasoning and hidden tool details stay private');
    assert.equal(commands.filter(c=>c.startsWith('/_send ')).length,2,'each response phase creates just one bubble');
    await pause(300);input('fixture followup');
    await until(()=>[...items.values()].some(i=>i.text==='Continuity confirmed.'&&!i.live),'same-contact followup');
    assert.equal(commands.filter(c=>c.startsWith('/_send ')).length,3);
  }finally{
    if(daemon){const stopped=once(daemon,'exit');daemon.kill('SIGTERM');await Promise.race([stopped,pause(5000)]);if(daemon.exitCode===null)daemon.kill('SIGKILL');}
    for(const client of vendor.clients)client.terminate();await new Promise(r=>vendor.close(r));
    model.closeAllConnections();await new Promise(r=>model.close(r));
    await rm(root,{recursive:true,force:true});
  }
});
