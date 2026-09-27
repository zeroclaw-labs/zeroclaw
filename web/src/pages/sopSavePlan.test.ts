import assert from 'node:assert/strict';
import test from 'node:test';

import { planSopSave, sopErrorText } from './sopSavePlan.ts';

test('a draft that was never loaded from disk is a create', () => {
  assert.deepEqual(planSopSave(null, 'deploy-prod'), { kind: 'create' });
});

test('an unchanged name is a plain save', () => {
  assert.deepEqual(planSopSave('deploy-prod', 'deploy-prod'), { kind: 'save' });
});

test('a changed name saves against the name the edit started under', () => {
  // `from` must be the editing name. Saving under the draft's new name would
  // write a second SOP and stand the original one up beside it, which is the
  // fork the daemon refuses to do on the caller's behalf.
  assert.deepEqual(planSopSave('deploy-prod', 'deploy-production'), {
    kind: 'save-then-rename',
    from: 'deploy-prod',
    to: 'deploy-production',
  });
});

test('a whitespace-only difference is a rename, not a no-op', () => {
  // The daemon decides which names are legal; the editor must not quietly
  // trim and store something the author did not type.
  assert.deepEqual(planSopSave('deploy', 'deploy '), {
    kind: 'save-then-rename',
    from: 'deploy',
    to: 'deploy ',
  });
});

test('a case-only change is a rename', () => {
  assert.deepEqual(planSopSave('deploy', 'Deploy'), {
    kind: 'save-then-rename',
    from: 'deploy',
    to: 'Deploy',
  });
});

test('an emptied name is still a rename, left for the daemon to reject', () => {
  assert.deepEqual(planSopSave('deploy', ''), {
    kind: 'save-then-rename',
    from: 'deploy',
    to: '',
  });
});

test('a SOP route error is reduced to the sentence the author can act on', () => {
  assert.equal(
    sopErrorText(new Error('API 409: {"error":"SOP \'deploy-staging\' already exists"}')),
    "SOP 'deploy-staging' already exists",
  );
});

test('a non-JSON API failure keeps its original text', () => {
  assert.equal(sopErrorText(new Error('API 500: Internal Server Error')), 'API 500: Internal Server Error');
});

test('an error that is not an API envelope is passed through untouched', () => {
  assert.equal(sopErrorText(new Error('Failed to fetch')), 'Failed to fetch');
});

test('a non-Error rejection is stringified rather than dropped', () => {
  assert.equal(sopErrorText('boom'), 'boom');
});
