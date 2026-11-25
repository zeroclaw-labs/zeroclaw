// The CLI owns physical message identities. This bounded, transient owner
// retains only references to the live items it created and serializes edits.
import {simplexTextMessages} from './format.mjs';

const positive = n => Number.isSafeInteger(n) && n > 0;
export class LiveTexts {
  constructor(rpc) {this.rpc = rpc; this.drafts = new Map(); this.orphans = new Map();}
  async start(key, contact, status) {
    if (typeof key !== 'string' || !/^[a-zA-Z0-9-]{1,128}$/.test(key)
      || !positive(Number(contact)) || typeof status !== 'string' || status.length > 1000)
      throw Object.assign(Error('simplex_invalid_live_draft'), {notDelivered:true});
    if (this.drafts.has(key) || this.drafts.size >= 32)
      throw Object.assign(Error('simplex_live_draft_limit'), {notDelivered:true});
    const draft = {contact:String(contact), items:[], text:'', tail:Promise.resolve(), closing:false};
    this.drafts.set(key, draft);
    try {await this.write(draft, status, true);}
    catch (error) {this.drafts.delete(key); throw error;}
  }
  async run(key, contact, action, terminal = false) {
    const draft = this.drafts.get(key);
    if (!draft || draft.contact !== String(contact) || draft.closing)
      throw Object.assign(Error('simplex_live_scope_mismatch'), {notDelivered:true});
    if (terminal) draft.closing = true;
    const work = draft.tail.then(() => action(draft));
    draft.tail = work.catch(() => {});
    try {return await work;}
    catch (error) {
      // An edit with a lost ACK can already be visible. The final caller must
      // never resend a whole answer because a live finalization was uncertain.
      if (terminal) error.noResend = true;
      throw error;
    } finally {if (terminal) this.drafts.delete(key);}
  }
  async create(draft, message, live) {
    const response = await this.rpc(`/_send @${draft.contact}${live ? ' live=on' : ''} json ${JSON.stringify([message])}`);
    const item = response.chatItems?.find(item => item.chatInfo?.type === 'direct'
      && String(item.chatInfo.contact?.contactId) === draft.contact
      && item.chatItem?.chatDir?.type === 'directSnd')?.chatItem;
    if (!positive(item?.meta?.itemId))
      throw Object.assign(Error('simplex_live_item_ack_missing'), {noResend:true});
    const owned = {id:item.meta.itemId, text:message.msgContent.text, live};
    draft.items.push(owned);
    return owned;
  }
  async edit(draft, item, text, live) {
    if (item.text === text && item.live === live) return;
    // Retrying an update uses the same platform item, so it cannot create a
    // duplicate. Creation is never retried after an ambiguous acknowledgement.
    const command = `/_update item @${draft.contact} ${item.id}${live ? ' live=on' : ''} json ${JSON.stringify({msgContent:{type:'text',text}, mentions:{}})}`;
    let response;
    try {response = await this.rpc(command);}
    catch (error) {
      if (error.notDelivered) throw error;
      response = await this.rpc(command);
    }
    if (!['chatItemUpdated','chatItemNotChanged'].includes(response.type))
      throw Object.assign(Error('simplex_live_update_ack_missing'), {noResend:true});
    item.text = text; item.live = live;
  }
  async write(draft, text, live) {
    if (typeof text !== 'string' || Buffer.byteLength(text) > 1024 * 1024)
      throw Object.assign(Error('simplex_live_text_limit'), {notDelivered:true});
    const messages = simplexTextMessages(text);
    if (messages.length > 128) throw Object.assign(Error('simplex_live_chunk_limit'), {notDelivered:true});
    for (const [index, message] of messages.entries()) {
      if (draft.items[index]) await this.edit(draft, draft.items[index], message.msgContent.text, live);
      else await this.create(draft, message, live);
    }
    // A final rewrite may be shorter than the preview. Remove only items owned
    // by this draft, never another response or a user's message.
    while (draft.items.length > messages.length) {
      const item = draft.items.at(-1);
      await this.rpc(`/_delete item @${draft.contact} ${item.id} broadcast`);
      draft.items.pop();
    }
  }
  async update(key, contact, text, status = false) {
    return this.run(key, contact, async draft => {
      if (status && draft.text) return;
      if (!status) draft.text = text;
      await this.write(draft, text, true);
    });
  }
  async flush(key, contact, text, receipt) {
    return this.run(key, contact, async draft => {
      if (draft.lastFlush === receipt) return;
      await this.write(draft, text, false);
      draft.items = []; draft.text = ''; draft.lastFlush = receipt;
    });
  }
  async finish(key, contact, text) {
    return this.run(key, contact, async draft => {await this.write(draft, text, false);}, true);
  }
  async cancel(key, contact, interrupted) {
    return this.run(key, contact, async draft => {
      await this.write(draft, draft.text ? `${draft.text}\n\n${interrupted}` : '', false);
    }, true);
  }
  async discover(emit, now = Date.now()) {
    // Reconnect recovery is metadata-only until native live authorization.
    // Only our sent live items from bounded recent direct-chat history qualify.
    const active = await this.rpc('/u');
    if (active.type !== 'activeUser' || !positive(active.user?.userId)) return;
    const chats = await this.rpc(`/_get chats ${active.user.userId} count=20`);
    for (const chat of (chats.chats ?? []).slice(0,20)) {
      const contact = chat.chatInfo?.contact?.contactId;
      if (chat.chatInfo?.type !== 'direct' || !positive(contact)) continue;
      const response = await this.rpc(`/_get chat @${contact} count=20`);
      for (const item of (response.chat?.chatItems ?? []).slice(-20)) {
        const created = Date.parse(item.meta?.createdAt);
        if (item.chatDir?.type !== 'directSnd' || item.meta?.itemLive !== true
          || !positive(item.meta?.itemId) || !(created <= now)
          || item.content?.type !== 'sndMsgContent' || item.content.msgContent?.type !== 'text'
          || this.orphans.size >= 64) continue;
        const key = `${contact}/${item.meta.itemId}`;
        this.orphans.set(key,{contact:String(contact), id:item.meta.itemId, text:item.content.msgContent.text});
        await emit({type:'live_orphan',contact:String(contact),item_id:item.meta.itemId});
      }
    }
  }
  async recover(contact, id, interrupted, placeholders) {
    const key = `${contact}/${id}`, item = this.orphans.get(key);
    if (!item) throw Object.assign(Error('simplex_live_recovery_scope_missing'),{notDelivered:true});
    const draft = {contact:String(contact)};
    // This text came from the CLI and is already in SimpleX's syntax. Do not
    // interpret it as Markdown a second time during reconnect cleanup.
    if (placeholders.includes(item.text))
      await this.rpc(`/_delete item @${contact} ${id} broadcast`);
    else await this.edit(draft,{...item,live:true},`${item.text}\n\n${interrupted}`,false);
    this.orphans.delete(key);
  }
}
