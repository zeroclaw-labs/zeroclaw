import assert from 'node:assert/strict';
import test from 'node:test';

import {
  CRON_DEFAULT_EXPRESSION,
  cronSchedulePatchForEdit,
  cronFieldValidity,
  getGuidedCronExpression,
  isValidCronExpression,
  isValidCronField,
  normalizeCronFields,
  splitCronExpression,
} from './cron.ts';

test('accepts the standard five-field expressions used by the cron editor', () => {
  assert.equal(CRON_DEFAULT_EXPRESSION, '0 9 * * 1-5');
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
  assert.equal(splitCronExpression('0 9 * *'), undefined);
  assert.equal(splitCronExpression('0 0 9 * * 1-5'), undefined);
  assert.equal(splitCronExpression('0 0 9 * * 1-5 2027'), undefined);
});

test('only valid five-field cron schedules use the guided editor', () => {
  assert.equal(
    getGuidedCronExpression({ kind: 'cron', expr: '0 9 * * 1-5' }),
    '0 9 * * 1-5',
  );
  assert.equal(
    getGuidedCronExpression({ kind: 'cron', expr: '0 0 9 * * 1-5' }),
    undefined,
  );
  assert.equal(
    getGuidedCronExpression({ kind: 'cron', expr: '0 0 9 * * 1-5 2027' }),
    undefined,
  );
  assert.equal(
    getGuidedCronExpression({ kind: 'cron', expr: '60 9 * * 1-5' }),
    undefined,
  );
  assert.equal(
    getGuidedCronExpression({ kind: 'at', at: '2030-01-01T00:00:00Z' }),
    undefined,
  );
  assert.equal(
    getGuidedCronExpression({ kind: 'every', every_ms: 3_600_000 }),
    undefined,
  );
});

test('partial edits omit schedule unless the existing schedule is guided cron', () => {
  assert.deepEqual(
    cronSchedulePatchForEdit(
      { kind: 'cron', expr: '0 9 * * 1-5' },
      '*/15 * * * *',
    ),
    { schedule: '*/15 * * * *' },
  );
  assert.deepEqual(
    cronSchedulePatchForEdit(
      { kind: 'cron', expr: '0 0 9 * * 1-5' },
      '0 9 * * 1-5',
    ),
    {},
  );
  assert.deepEqual(
    cronSchedulePatchForEdit(
      { kind: 'at', at: '2030-01-01T00:00:00Z' },
      '0 9 * * 1-5',
    ),
    {},
  );
  assert.deepEqual(
    cronSchedulePatchForEdit(
      { kind: 'every', every_ms: 3_600_000 },
      '0 9 * * 1-5',
    ),
    {},
  );
});
