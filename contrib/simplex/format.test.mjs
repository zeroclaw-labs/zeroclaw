import {test} from 'node:test';
import assert from 'node:assert/strict';
import {simplexTextMessages} from './format.mjs';
const text = source => simplexTextMessages(source).map(message => message.msgContent.text).join('');

test('standard Markdown emphasis becomes SimpleX emphasis', () => {
  assert.equal(text('**Bold** and __strong__, *italic*, _emphasis_, ~~removed~~.'),
    '*Bold* and *strong*, _italic_, _emphasis_, ~removed~.');
  assert.equal(text('**bold with *italic* inside**'), '*bold with italic inside*');
  assert.equal(text('Status:**ready**'), 'Status: *ready*');
  assert.equal(text('word**middle**word'), 'wordmiddleword');
});

test('code, escaped syntax, URLs and raw HTML are not rewritten as emphasis', () => {
  for (const source of [
    'Use `**literal**` and `a_b`.',
    '```js\nconst value = "**literal**";\n```\n',
    '    const value = "**literal**";\n',
    'https://example.test/a_b?x=1&y=2',
    String.raw`\*literal\* and snake_case_identifier`,
    '<script>**literal**</script>\n',
  ]) assert.equal(text(source), source);
});

test('headings, lists, task items, quotes and tables have readable fallbacks', () => {
  assert.equal(text('## Heading\n\n- **First**\n- Second'), '*Heading*\n\n• *First*\n• Second\n');
  assert.equal(text('1. **One**\n2. [ ] Task'), '1. *One*\n2. [ ] Task\n');
  assert.equal(text('> **Quote**\n'), '> *Quote*\n');
  assert.equal(text('| A | B |\n| - | - |\n| **x** | y |'), '*A* | *B*\n*x* | y\n');
});

test('links keep their destination visible and do not execute HTML', () => {
  assert.equal(text('[Documentation](https://example.test/a_b) and ![diagram](https://example.test/image.png)'),
    'Documentation (https://example.test/a_b) and diagram (https://example.test/image.png)');
});

test('simple math arrows become readable Unicode within prose and emphasis', () => {
  assert.equal(text(String.raw`**Plan $\rightarrow$ Dispatch $\to$ Review $\Rightarrow$ Refine**`),
    '*Plan → Dispatch → Review ⇒ Refine*');
  assert.equal(text(String.raw`$a \leftrightarrow b$; $$\Leftarrow$$; \(\mapsto\); \[\downarrow\]`),
    'a ↔ b; ⇐; ↦; ↓');
  assert.equal(text(String.raw`$\leftarrow$ $\Leftrightarrow$ $\uparrow$`), '← ⇔ ↑');
});

test('math conversion preserves code, destinations, escapes, currency and unsupported TeX', () => {
  for (const source of [
    '`$\\rightarrow$`',
    '```tex\n$\\rightarrow$\n```\n',
    String.raw`$\frac{a}{b} \rightarrow c$`,
    String.raw`\(\unknown\)`,
    String.raw`$x_1 \rightarrow y^2$`,
    String.raw`$\rightarrowtail$`,
    String.raw`\$\rightarrow\$`,
    '$5 and $10',
    'https://example.test/$to$/file',
  ]) assert.equal(text(source), source);
  assert.equal(text(String.raw`[Docs](https://example.test/$\rightarrow$)`),
    String.raw`Docs (https://example.test/$\rightarrow$)`);
});

test('mixed headings retain one readable bold span and rules use a text separator', () => {
  assert.equal(text('### Updating `PROFILE.md` (now)\n\n---\n\nReady.'),
    '*Updating PROFILE.md (now)*\n\n────────\n\nReady.');
  assert.equal(text('### Update [`PROFILE.md`](https://example.test/profile)\n'),
    '*Update PROFILE.md* (https://example.test/profile)\n');
  assert.equal(text('***\n'), '────────\n');
  assert.equal(text('    ---\n'), '    ---\n');
});

test('large Unicode replies close formatting in each bounded message without losing content', () => {
  const content = 'Words 😀 and text '.repeat(2500).trim();
  const messages = simplexTextMessages(`**${content}**`);
  assert.ok(messages.length > 1);
  for (const {msgContent} of messages) {
    assert.ok([...msgContent.text].length <= 12000);
    const core = msgContent.text.trim();
    assert.match(core, /^\*\S[\s\S]*\S\*$/);
    assert.ok(!/[\uD800-\uDBFF]$/.test(msgContent.text));
  }
  assert.equal(messages.map(({msgContent}) => msgContent.text.replaceAll('*', '')).join(''), content);
});

test('short inline code stays together at a message boundary and long code keeps its source', () => {
  const source = `${'a'.repeat(11995)} \`**literal**\``;
  const messages = simplexTextMessages(source);
  assert.equal(messages[1].msgContent.text, '`**literal**`');
  assert.equal(messages.map(message => message.msgContent.text).join(''), source);
  const code = '```\n' + '😀'.repeat(24000) + '\n```';
  assert.equal(text(code), code);
  assert.ok(simplexTextMessages(code).every(message => [...message.msgContent.text].length <= 12000));
  assert.deepEqual(simplexTextMessages(''), []);
});
