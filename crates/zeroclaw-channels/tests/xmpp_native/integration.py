"""Actual encrypted XmppChannel + shared runtime on loopback synthetic accounts."""
import argparse,asyncio,atexit,base64,hashlib,json,logging,os,pathlib,signal,ssl,tempfile,time
from xml.etree import ElementTree as ET
from types import SimpleNamespace
from slixmpp import ClientXMPP
from slixmpp.exceptions import IqError
from slixmpp.xmlstream.handler import CoroutineCallback
from slixmpp.xmlstream.matcher import MatchXPath
from reference_fixture import MemoryStorage
from oldmemo import Oldmemo
from oldmemo.etree import parse_message
from omemo.identity_key_pair import IdentityKeyPair
logging.disable(logging.CRITICAL)
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--server-root',type=pathlib.Path,required=True,help='Private synthetic Prosody fixture directory containing certs/cert.pem and fixture-accounts.json')
parser.add_argument('--output',type=pathlib.Path,required=True,help='Untracked output report path')
options=parser.parse_args()
root=pathlib.Path(__file__).resolve().parent
workspace=root.parents[3]
server=options.server_root.resolve(strict=True)
accounts=json.loads((server/'fixture-accounts.json').read_text())
assert set(accounts)>={'bot','bob','alice','mallory'}
summary={'fixture_only':True,'result':'running','outcomes':[],'production_changed':False}
summary['source_sha256']={str(p.relative_to(workspace)):hashlib.sha256(p.read_bytes()).hexdigest() for p in (workspace/'crates/zeroclaw-channels/src').rglob('xmpp*.rs')}
for p in (workspace/'crates/zeroclaw-channels/src/xmpp_omemo').rglob('*.rs'):
    summary['source_sha256'][str(p.relative_to(workspace))]=hashlib.sha256(p.read_bytes()).hexdigest()
out=options.output.resolve()

def finish():
    if summary['result']!='passed':summary['result']='failed'
    summary['completed_utc']=time.strftime('%Y-%m-%dT%H:%M:%SZ',time.gmtime());out.write_text(json.dumps(summary,indent=2)+'\n')
atexit.register(finish);finish();summary['result']='running'
async def publish_fixture(client,node,content):
    iq=client.Iq();iq['type']='set';iq['to']=client.boundjid.bare
    pubsub=ET.Element('{http://jabber.org/protocol/pubsub}pubsub')
    publish=ET.SubElement(pubsub,'{http://jabber.org/protocol/pubsub}publish',{'node':node})
    item=ET.SubElement(publish,'{http://jabber.org/protocol/pubsub}item',{'id':'current'});item.append(content)
    options=ET.SubElement(pubsub,'{http://jabber.org/protocol/pubsub}publish-options')
    form=ET.SubElement(options,'{jabber:x:data}x',{'type':'submit'})
    for name,value in [('FORM_TYPE','http://jabber.org/protocol/pubsub#publish-options'),('pubsub#access_model','open')]:
        field=ET.SubElement(form,'{jabber:x:data}field',{'var':name});ET.SubElement(field,'{jabber:x:data}value').text=value
    iq.append(pubsub);await iq.send(timeout=10)

async def native_barrier(client):
    # A bare-JID ping can be answered by the server before the client handles
    # preceding traffic. Address the bound native resource as the barrier.
    iq=client.Iq();iq['type']='get';iq['to']='bob@example.test/zeroclaw'
    iq.append(ET.Element('{urn:xmpp:ping}ping'))
    await iq.send(timeout=12)

async def remove_fixture_bundle(client):
    iq=client.Iq();iq['type']='set';iq['to']=client.boundjid.bare
    owner=ET.Element('{http://jabber.org/protocol/pubsub#owner}pubsub')
    ET.SubElement(owner,'{http://jabber.org/protocol/pubsub#owner}delete',{'node':'eu.siacs.conversations.axolotl.bundles:101'})
    iq.append(owner)
    try:await iq.send(timeout=10)
    except IqError as error:
        if error.iq['error']['condition']!='item-not-found':raise
    iq=client.Iq();iq['type']='get';iq['to']=client.boundjid.bare
    pubsub=ET.Element('{http://jabber.org/protocol/pubsub}pubsub')
    ET.SubElement(pubsub,'{http://jabber.org/protocol/pubsub}items',{'node':'eu.siacs.conversations.axolotl.bundles:101'})
    iq.append(pubsub)
    try:
        result=await iq.send(timeout=10)
        assert not result.xml.findall('.//{http://jabber.org/protocol/pubsub}item'),'Fixture peer bundle still exists'
    except IqError as error:
        if error.iq['error']['condition']!='item-not-found':raise

def rejected_prekey_packet(wire,variant):
    # Mutate the named protobuf field in a synthetic reference packet. The
    # outer packet can end in registration metadata rather than the inner MAC.
    offset=1
    def varint():
        nonlocal offset
        value=0;shift=0
        while True:
            byte=wire[offset];offset+=1;value|=(byte&127)<<shift
            if byte<128:return value
            shift+=7
            assert shift<64
    while offset<len(wire):
        start=offset;tag=varint()
        if tag&7==0:varint()
        elif tag&7==2:
            length=varint();offset+=length
        else:raise AssertionError('Unexpected synthetic wire type')
        if tag==8 and variant=='no-one-time-prekey':return wire[:start]+wire[offset:]
        if tag==34 and variant=='bad-key-authentication':
            altered=bytearray(wire);altered[offset-1]^=1;return bytes(altered)
    raise AssertionError('Synthetic first contact lacks the requested field')

async def seed_public_fixture(f,mode,port):
    ready=asyncio.get_running_loop().create_future()
    context=ssl.create_default_context(cafile=str(server/'certs/cert.pem'))
    client=ClientXMPP('bob@example.test/public-fixture-setup',accounts['bob'],ssl_context=context)
    client.enable_direct_tls=mode=='direct';client.enable_starttls=mode=='starttls';client.enable_plaintext=False
    async def started(_):
        try:
            await publish_fixture(client,'eu.siacs.conversations.axolotl.bundles:202',ET.fromstring(f['reference_bundle_xml']))
            await publish_fixture(client,'eu.siacs.conversations.axolotl.devicelist',ET.fromstring("<list xmlns='eu.siacs.conversations.axolotl'><device id='303' label='Outro cliente existente'/></list>"))
            ready.set_result(True)
        except Exception as error:ready.set_exception(error)
    client.add_event_handler('session_start',started);client.connect(host='127.0.0.1',port=port)
    try:await asyncio.wait_for(ready,12)
    finally:client.disconnect()

async def main():
    boundary_tests = ['native_ingress_admission_precedes_hooks_and_agent_effects', 'native_ingress_runtime_reference_preserves_original_receipt_through_hook']
    env = os.environ.copy(); # Respect the caller's existing Cargo target directory.
    boundary = await asyncio.create_subprocess_exec('cargo','test','--locked','-j4','-p','zeroclaw-channels','--no-default-features','--features','channel-xmpp','native_ingress_','--','--nocapture',cwd=workspace,env=env,stdout=asyncio.subprocess.PIPE,stderr=asyncio.subprocess.PIPE)
    stdout, stderr = await boundary.communicate()
    if boundary.returncode or any((name+' ... ok').encode() not in stdout for name in boundary_tests):
        raise RuntimeError('Shared ingress boundary regression failed')
    summary['boundary_tests_passed'] = boundary_tests
    build=await asyncio.create_subprocess_exec('cargo','test','--locked','-j4','-p','zeroclaw-channels','--no-default-features','--features','channel-xmpp','--lib','--no-run',cwd=workspace,env=env)
    if await build.wait():raise RuntimeError('Native test executable did not build')
    with tempfile.TemporaryDirectory(prefix='zeroclaw-omemo-channel-',dir='/var/tmp') as tmp:
        folder=pathlib.Path(tmp);os.chmod(folder,0o700)
        for mode,port,fresh in [('direct',15223,False),('direct',15223,True),('starttls',15222,False),('starttls',15222,True)]:
            label=mode+('-fresh' if fresh else '-existing')
            fixture=folder/(label+'.json');report=folder/(label+'-native.json')
            create=await asyncio.create_subprocess_exec(os.sys.executable,str(root/'reference_fixture.py'),str(fixture),'--payload',*(['--fresh-db'] if fresh else []),stdout=asyncio.subprocess.DEVNULL)
            assert await create.wait()==0
            f=json.loads(fixture.read_text());await seed_public_fixture(f,mode,port);env=os.environ.copy()
            env.update(SSL_CERT_FILE=str(server/'certs/cert.pem'),ZEROCLAW_NATIVE_OMEMO_FIXTURE=str(fixture),ZEROCLAW_NATIVE_OMEMO_RESULT=str(report),ZEROCLAW_NATIVE_OMEMO_ACCOUNTS=str(server/'fixture-accounts.json'),ZEROCLAW_NATIVE_OMEMO_TLS_MODE=mode,ZEROCLAW_NATIVE_OMEMO_FRESH="1" if fresh else "0")
            native=await asyncio.create_subprocess_exec('cargo','test','--locked','-j4','-p','zeroclaw-channels','--no-default-features','--features','channel-xmpp','native_omemo_actual_channel_shared_runtime_and_socket_ack','--','--ignored','--nocapture',cwd=workspace,env=env,stdout=asyncio.subprocess.PIPE,stderr=asyncio.subprocess.PIPE,start_new_session=True)
            clients=[]
            try:
                for _ in range(300):
                    if report.exists() and json.loads(report.read_text()).get('ready'):break
                    if native.returncode is not None:
                        stdout,stderr=await native.communicate()
                        detail=(stdout+stderr).decode(errors='replace')[-4000:]
                        for password in accounts.values():detail=detail.replace(password,'[redacted]')
                        raise RuntimeError('Actual channel failed before ready: '+detail)
                    await asyncio.sleep(.05)
                else:raise RuntimeError('Actual channel readiness deadline')
                ready={name:asyncio.get_running_loop().create_future() for name in ['alice','mallory']}
                done=asyncio.get_running_loop().create_future()
                store=MemoryStorage();store.values=f['alice_storage_after_send'];alice=Oldmemo(store)
                session=await alice.load_session('bob@example.test',202)
                bs=MemoryStorage();bs.values=f['bob_storage'];identity=await IdentityKeyPair.get(bs)
                class Manager:
                    async def get_device_information(self,jid):
                        assert jid=='bob@example.test'
                        return [SimpleNamespace(device_id=202,identity_key=identity.identity_key)]
                class Client(ClientXMPP):
                    def __init__(self,name):
                        context=ssl.create_default_context(cafile=str(server/'certs/cert.pem'));context.minimum_version=ssl.TLSVersion.TLSv1_2
                        super().__init__(name+'@example.test/channel-lab',accounts[name],ssl_context=context)
                        self.name=name;self.enable_direct_tls=mode=='direct';self.enable_starttls=mode=='starttls';self.enable_plaintext=False
                        self.add_event_handler('session_start',self.started)
                        if name=='alice':self.register_handler(CoroutineCallback('Actual channel encrypted response',MatchXPath('{jabber:client}message'),self.received))
                    async def started(self,_):
                        assert self.transport.get_extra_info('ssl_object') is not None
                        self.send_presence();ready[self.name].set_result(True)
                    async def received(self,message):
                        try:
                            if message['from'].bare!='bob@example.test':return
                            encrypted=message.xml.find('{eu.siacs.conversations.axolotl}encrypted')
                            assert encrypted is not None and not str(message['body'])
                            parsed=await parse_message(encrypted,'bob@example.test','alice@example.test',Manager())
                            key,exchange=next(iter(parsed.keys));assert exchange is None
                            material=await alice.decrypt_key_material(session,key)
                            assert await alice.decrypt_plaintext(parsed.content,material)==b'ok'
                            if not done.done():done.set_result(True)
                        except Exception as error:
                            if not done.done():done.set_exception(error)
                for name in ['alice','mallory']:
                    client=Client(name);clients.append(client);client.connect(host='127.0.0.1',port=port)
                await asyncio.wait_for(asyncio.gather(*ready.values()),10)
                by_name={c.name:c for c in clients}
                if fresh:
                    await remove_fixture_bundle(by_name['alice'])
                    await publish_fixture(by_name['alice'],'eu.siacs.conversations.axolotl.devicelist',ET.fromstring("<list xmlns='eu.siacs.conversations.axolotl'><device id='101'/></list>"))
                    unavailable=by_name['alice'].make_message(mto='bob@example.test',mtype='chat')
                    unavailable['id']='missing-peer-bundle';unavailable.xml.append(ET.fromstring(f['first_xml']));unavailable.send()
                    await native_barrier(by_name['alice'])
                    await publish_fixture(by_name['alice'],'eu.siacs.conversations.axolotl.bundles:101',ET.fromstring(f['alice_bundle_xml']))
                for name in ['mallory','alice']:
                    message=by_name[name].make_message(mto='bob@example.test',mbody='must never reach agent',mtype='chat');message.send()
                forged=by_name['mallory'].make_message(mto='bob@example.test',mtype='chat');forged['id']='forged';forged.xml.append(ET.fromstring(f['first_xml' if fresh else 'next_xml']));forged.send()
                # Allowed accounts can send traffic for another recipient/device.
                # Such untrusted input must not reconnect the channel or drop replies.
                for variant in ('missing-recipient', 'unknown-device'):
                    payload=ET.fromstring(f['first_xml' if fresh else 'next_xml'])
                    header=payload.find('{eu.siacs.conversations.axolotl}header')
                    if variant=='missing-recipient':
                        for key in header.findall('{eu.siacs.conversations.axolotl}key'):key.set('rid','999999')
                    else:header.set('sid','999999')
                    rejected=by_name['alice'].make_message(mto='bob@example.test',mtype='chat')
                    rejected['id']='rejected-'+variant;rejected.xml.append(payload);rejected.send()
                if fresh:
                    for variant in ('no-one-time-prekey','bad-key-authentication'):
                        payload=ET.fromstring(f['first_xml']);key=payload.find('.//{eu.siacs.conversations.axolotl}key')
                        wire=base64.b64decode(key.text)
                        wire=rejected_prekey_packet(wire,variant)
                        key.text=base64.b64encode(wire).decode()
                        rejected=by_name['alice'].make_message(mto='bob@example.test',mtype='chat')
                        rejected['id']='rejected-'+variant;rejected.xml.append(payload);rejected.send()
                await native_barrier(by_name['alice'])
                message=by_name['alice'].make_message(mto='bob@example.test',mtype='chat');message['id']='actual-channel-inbound';message.xml.append(ET.fromstring(f['first_xml' if fresh else 'next_xml']));message.send()
                duplicate=by_name['alice'].make_message(mto='bob@example.test',mtype='chat');duplicate['id']='actual-channel-inbound';duplicate.xml.append(ET.fromstring(f['first_xml' if fresh else 'next_xml']));duplicate.send()
                try:await asyncio.wait_for(done,15)
                except Exception:
                    if native.returncode is not None:
                        stdout,stderr=await native.communicate()
                        detail=(stdout+stderr).decode(errors='replace')[-4000:]
                        for password in accounts.values():detail=detail.replace(password,'[redacted]')
                        print(detail)
                    raise
                stdout,stderr=await asyncio.wait_for(native.communicate(),12)
                if native.returncode:
                    detail=(stdout+stderr).decode(errors='replace')[-4000:]
                    for password in accounts.values():detail=detail.replace(password,'[redacted]')
                    raise RuntimeError('Actual channel test failed: '+detail)
                result=json.loads(report.read_text());assert result['result']=='passed'
                iq=by_name['alice'].Iq();iq['type']='get';iq['to']='bob@example.test'
                iq.append(ET.fromstring("<pubsub xmlns='http://jabber.org/protocol/pubsub'><items node='eu.siacs.conversations.axolotl.bundles:202'/></pubsub>"))
                response=await iq.send(timeout=10)
                from oldmemo.etree import parse_bundle
                published=parse_bundle(response.xml.find('.//{eu.siacs.conversations.axolotl}bundle'),'bob@example.test',202)
                baseline=parse_bundle(ET.fromstring(f['reference_bundle_xml']),'bob@example.test',202)
                assert published.identity_key==baseline.identity_key and published.signed_pre_key_id==baseline.signed_pre_key_id
                actual_xml=response.xml.find('.//{eu.siacs.conversations.axolotl}bundle');baseline_xml=ET.fromstring(f['reference_bundle_xml'])
                for name in ['signedPreKeyPublic','signedPreKeySignature','identityKey']:
                    a=actual_xml.find('{eu.siacs.conversations.axolotl}'+name);b=baseline_xml.find('{eu.siacs.conversations.axolotl}'+name)
                    assert a.text==b.text and a.attrib==b.attrib
                assert len(published.pre_key_ids)==100
                if fresh:
                    from oldmemo.oldmemo import KeyExchangeImpl
                    exchange,_=KeyExchangeImpl.parse(bytes(f['fresh_wire']),False)
                    assert exchange.pre_key_id not in published.pre_key_ids.values()
                result['published_prekeys_verified']=True
                iq=by_name['alice'].Iq();iq['type']='get';iq['to']='bob@example.test'
                iq.append(ET.fromstring("<pubsub xmlns='http://jabber.org/protocol/pubsub'><items node='eu.siacs.conversations.axolotl.devicelist'/></pubsub>"))
                response=await iq.send(timeout=10)
                devices={int(d.get('id')):d.get('label') for d in response.xml.findall('.//{eu.siacs.conversations.axolotl}device')}
                assert devices=={202:None,303:'Outro cliente existente'}
                result['other_devices_and_labels_preserved']=True
                summary['outcomes'].append(dict(tls_mode=mode,python_decrypted_actual_runtime_reply=True,**{k:v for k,v in result.items() if k!='tls_mode'}))
            finally:
                for client in clients:client.disconnect()
                if native.returncode is None:
                    os.killpg(native.pid,signal.SIGTERM)
                    try:await asyncio.wait_for(native.communicate(),3)
                    except asyncio.TimeoutError:os.killpg(native.pid,signal.SIGKILL);await native.communicate()
    summary['full_agent_router_tested']=True;summary['shared_dispatch_loop_tested']=True;summary['result']='passed';finish();print('ACTUAL_CHANNEL_SHARED_RUNTIME_ENCRYPTED_SOCKET_ACK_REVOCATION_AND_CANCELLATION_PASSED')
if __name__=='__main__':asyncio.run(main())
