import assert from 'node:assert/strict';
import test from 'node:test';

import {
  chatInputFocusOwner,
  nextChatInputRefocus,
  type ChatInputRefocusState,
} from './chatInputRefocus.logic.ts';

const ended: ChatInputRefocusState = {
  armed: true,
  typing: false,
  inputEnabled: true,
  inputVisible: true,
  focusOwner: 'nothing',
};

test('a running turn arms the refocus', () => {
  assert.equal(nextChatInputRefocus({ ...ended, armed: false, typing: true }), 'arm');
  assert.equal(nextChatInputRefocus({ ...ended, typing: true, inputEnabled: false }), 'arm');
});

test('the message box takes focus back when the turn ends', () => {
  assert.equal(nextChatInputRefocus(ended), 'focus');
  assert.equal(nextChatInputRefocus({ ...ended, focusOwner: 'input' }), 'focus');
});

test('nothing happens without a turn having run', () => {
  assert.equal(nextChatInputRefocus({ ...ended, armed: false }), 'none');
});

test('a still-disabled box keeps the refocus armed', () => {
  assert.equal(nextChatInputRefocus({ ...ended, inputEnabled: false }), 'wait');
});

test('focus moved elsewhere during the turn is left alone', () => {
  assert.equal(nextChatInputRefocus({ ...ended, focusOwner: 'other' }), 'skip');
});

test('a turn ending in a hidden chat tab does not move focus', () => {
  assert.equal(nextChatInputRefocus({ ...ended, inputVisible: false }), 'skip');
});

test('focus owner classification', () => {
  const body = {};
  const input = {};
  const button = {};
  assert.equal(chatInputFocusOwner(body, body, input), 'nothing');
  assert.equal(chatInputFocusOwner(null, body, input), 'nothing');
  assert.equal(chatInputFocusOwner(input, body, input), 'input');
  assert.equal(chatInputFocusOwner(button, body, input), 'other');
});
