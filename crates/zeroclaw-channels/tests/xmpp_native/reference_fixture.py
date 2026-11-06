"""Synthetic reference fixture; never reads production identities or accounts."""
import asyncio, json, os, pathlib, sys
from omemo.storage import Storage, Just, Nothing
from omemo.identity_key_pair import IdentityKeyPair
from oldmemo import Oldmemo
from oldmemo.oldmemo import PlainKeyMaterialImpl
import xeddsa

class MemoryStorage(Storage):
    def __init__(self):
        super().__init__()
        self.values = {}
    async def _load(self, key):
        return Just(self.values[key]) if key in self.values else Nothing()
    async def _store(self, key, value):
        self.values[key] = value
    async def _delete(self, key):
        self.values.pop(key, None)

async def main():
    a_store, b_store = MemoryStorage(), MemoryStorage()
    alice, bob = Oldmemo(a_store), Oldmemo(b_store)
    await bob.generate_pre_keys(5)
    a_peer,a_id,b_peer,b_id=('alice@example.test',101,'bob@example.test',202) if '--active' in sys.argv else ('bob@example.test',202,'alice@example.test',101)
    bundle = await bob.get_bundle(a_peer, a_id)
    from oldmemo.etree import serialize_bundle
    from xml.etree import ElementTree as ET
    reference_bundle_xml=ET.tostring(serialize_bundle(bundle),encoding='unicode')
    initial = PlainKeyMaterialImpl(bytes(range(16)), bytes(range(16,32)))
    if '--fresh-db' in sys.argv:
        first_content,initial=await alice.encrypt_plaintext('Primeiro contato — prekey transacional'.encode())
    a_session, first = await alice.build_session_active(a_peer, a_id, bundle, initial)
    if '--rotated' in sys.argv:
        await bob.rotate_signed_pre_key()
        await bob.get_bundle(a_peer,a_id)
    if '--fresh-recipient' in sys.argv or '--fresh-db' in sys.argv:
        from copy import deepcopy
        fresh_state = deepcopy(b_store.values['/'+bob.namespace+'/x3dh'])
        fresh_storage=deepcopy(b_store.values)
        fresh_wire, _ = a_session.key_exchange.serialize(first.serialize())
    b_session, decoded = await bob.build_session_passive(b_peer, b_id, a_session.key_exchange, first)
    assert decoded.key + decoded.auth_tag == initial.key + initial.auth_tag
    await bob.store_session(b_session)
    if '--active' in sys.argv:
        await alice.store_session(a_session)
        alice,bob= bob,alice
        a_store,b_store=b_store,a_store
        a_session,b_session=b_session,a_session
    next_material = PlainKeyMaterialImpl(bytes([17])*16, bytes([34])*16)
    if '--payload' in sys.argv:
        content, next_material = await alice.encrypt_plaintext('Olá do Python — sessão preservada'.encode())
    second = await alice.encrypt_key_material(a_session, next_material)
    if '--skipped' in sys.argv:
        late = second
        remaining = await alice.encrypt_key_material(a_session, PlainKeyMaterialImpl(bytes([18])*16, bytes([35])*16))
        newest = await alice.encrypt_key_material(a_session, PlainKeyMaterialImpl(bytes([19])*16, bytes([36])*16))
        await bob.decrypt_key_material(b_session, newest)
        await bob.store_session(b_session)
        second = late
    await alice.store_session(a_session)
    await alice.generate_pre_keys(5)
    alice_bundle=await alice.get_bundle('alice@example.test',101)
    alice_bundle_xml=ET.tostring(serialize_bundle(alice_bundle),encoding='unicode')
    b_identity = (await IdentityKeyPair.get(b_store)).as_priv()
    fixture = {"fixture_only":True,"alice_bundle_xml":alice_bundle_xml,"reference_bundle_xml":reference_bundle_xml, "bob_storage":b_store.values,
        "alice_storage_after_send":a_store.values,
        "bob_ratchet":b_session.double_ratchet.json,
        "bob_identity_private":list(b_identity.priv),
        "bob_identity_curve":list(xeddsa.ed25519_pub_to_curve25519_pub(b_identity.identity_key)),
        "alice_identity_curve":list(xeddsa.ed25519_pub_to_curve25519_pub((await IdentityKeyPair.get(a_store)).identity_key)),
        "next_wire":list(second.serialize()),
        "expected_next_material":list(next_material.key+next_material.auth_tag)}
    if '--skipped' in sys.argv:
        fixture['remaining_delayed_wire'] = list(remaining.serialize())
        fixture['remaining_delayed_material'] = list(bytes([18])*16+bytes([35])*16)
    if '--payload' in sys.argv:
        fixture['payload'] = list(content.ciphertext)
        fixture['iv'] = list(content.initialization_vector)
        fixture['expected_plaintext'] = list('Olá do Python — sessão preservada'.encode())
        from oldmemo.etree import serialize_message
        from omemo.message import Message
        from xml.etree import ElementTree as ET
        fixture['next_xml']=ET.tostring(serialize_message(Message(bob.namespace,'alice@example.test',101,content,frozenset([(second,None)]))),encoding='unicode')
    if '--fresh-recipient' in sys.argv or '--fresh-db' in sys.argv:
        fixture['fresh_wire'] = list(fresh_wire)
        fixture['expected_initial_material'] = list(initial.key+initial.auth_tag)
        fixture['signed_prekey_private'] = list(fresh_state['signed_pre_key']['priv'].encode('latin1'))
        fixture['prekey_privates'] = {str(bundle.pre_key_ids[xeddsa.priv_to_curve25519_pub(priv.encode('latin1'))]):list(priv.encode('latin1')) for priv in fresh_state['pre_keys']}
    if '--fresh-db' in sys.argv:
        fixture.update(fresh_storage=fresh_storage,first_payload=list(first_content.ciphertext),
            first_iv=list(first_content.initialization_vector),expected_first=list('Primeiro contato — prekey transacional'.encode()),
            alice_identity_ed=list((await IdentityKeyPair.get(a_store)).identity_key))
        fixture['first_xml']=ET.tostring(serialize_message(Message(bob.namespace,'alice@example.test',101,first_content,frozenset([(first,a_session.key_exchange)]))),encoding='unicode')
    root=pathlib.Path("/var/tmp/zeroclaw-native-omemo-fixtures");root.mkdir(exist_ok=True,mode=0o700)
    path=pathlib.Path(sys.argv[1]) if len(sys.argv)>1 else root/"established.json"
    with open(path,'w',opener=lambda p,f:os.open(p,f,0o600)) as f:json.dump(fixture,f)
    def shape(v):
        if isinstance(v,dict):return {k:shape(x) for k,x in v.items()}
        if isinstance(v,list):return {"list_items":len(v)}
        if isinstance(v,str):return "string:"+str(len(v))
        return type(v).__name__
    print(json.dumps({"reference_fixture_created":True,"ratchet_shape":shape(fixture['bob_ratchet'])}))
if __name__ == '__main__':asyncio.run(main())
