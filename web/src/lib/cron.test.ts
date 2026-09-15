import assert from 'node:assert/strict';
import test from 'node:test';

import {
  CRON_DEFAULT_EXPRESSION,
  cronFieldValidity,
  isValidCronExpression,
  isValidCronField,
  normalizeCronFields,
  splitCronExpression,
} from './cron.ts';

test('accepts the standard five-field expressions used by the cron editor', () => {
  assert.equal(isValidCronExpression('*/15 * * * *'), true);
  assert.equal(isValidCronExpression('0 9 * * 1-5'), true);
  assert.equal(isValidCronExpression('0 0 1 * *'), true);
  assert.equal(isValidCronExpression('0 9 1,15 * 0,6'), true);
});

test('rejects out-of-range, empty, and unsupported field syntax', () => {
  assert.equal(isValidCronExpression('60 * * * *'), false);
  assert.equal(isValidCronExpression('0 24 * * *'), false);
  assert.equal(isValidCronExpression('0 9 0 * *'), false);
  assert.equal(isValidCronExpression('0 9 * 13 *'), false);
  assert.equal(isValidCronExpression('0 9 * * 8'), false);
  assert.equal(isValidCronExpression('0 9 * *'), false);
  assert.equal(isValidCronExpression('0 9 * * * *'), false);
  assert.equal(isValidCronExpression('0 9 * * */0'), false);
  assert.equal(isValidCronExpression('0 9 * 5-2 *'), false);
  assert.equal(isValidCronExpression('0 9 * * 1,,5'), false);
});

test('reports field-level validity for the five editor inputs', () => {
  assert.deepEqual(cronFieldValidity(['0', '9', '*', '13', '1-5']), [true, true, true, false, true]);
  assert.equal(isValidCronField('*/15', 0), true);
  assert.equal(isValidCronField('60', 0), false);
  assert.equal(isValidCronField('1-5/2', 4), true);
});

test('normalizes and parses editor fields without changing the API shape', () => {
  assert.equal(normalizeCronFields(['*/15', ' *', '* ', ' *', '1-5']), '*/15 * * * 1-5');
  assert.deepEqual(splitCronExpression('0 9 * * 1-5'), ['0', '9', '*', '*', '1-5']);
  assert.deepEqual(splitCronExpression('0 9 * *'), CRON_DEFAULT_EXPRESSION.split(' '));
});
