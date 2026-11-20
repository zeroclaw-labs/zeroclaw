import { readFileSync } from "node:fs";
import vm from "node:vm";
import assert from "node:assert/strict";
import test from "node:test";

const source = readFileSync(new URL("./browser_credential_form.js", import.meta.url), "utf8");
const origin = "https://login.example.test";

function fixture({ action = origin + "/submit", method = "post", top = true, maskedOtp = false, duplicateOtp = false } = {}) {
  const sent = [];
  class Form { submit() { sent.push(fields.map(field => field.value)); } }
  class Input {
    value = "";
    getClientRects() { return [1]; }
  }
  const form = Object.assign(new Form(), { action, method, target: "" });
  const fields = Array.from({ length: 3 }, () => Object.assign(new Input(), { form }));
  const window = { location: { origin } };
  window.top = top ? window : {};
  const document = {
    baseURI: origin,
    querySelectorAll(selector) {
      if (selector.startsWith('input[type="password"]')) {
        return maskedOtp && selector.includes(':not([name="otp"])') ? [] : [fields[1]];
      }
      if (selector.includes('one-time-code')) {
        const matches = !maskedOtp || selector.includes('input[name="otp"]') ? [fields[2]] : [];
        return duplicateOtp ? [...matches, Object.assign(new Input(), { form })] : matches;
      }
      return [fields[0]];
    },
  };
  const run = vm.runInNewContext(`(${source})`, { window, document, URL, HTMLInputElement: Input, HTMLFormElement: Form });
  return { run: values => run({ origin, values }), sent, fields };
}

test("submits synthetic login and clears input values", () => {
  const { run, sent, fields } = fixture();
  assert.equal(run({ username: "fixture", password: "sentinel" }), true);
  assert.deepEqual(sent, [["fixture", "sentinel", ""]]);
  assert.deepEqual(fields.map(field => field.value), ["", "", ""]);
});

test("submits only the one-time code", () => {
  const { run, sent, fields } = fixture();
  assert.equal(run({ otp: "123456" }), true);
  assert.deepEqual(sent, [["", "", "123456"]]);
  assert.deepEqual(fields.map(field => field.value), ["", "", ""]);
});

for (const options of [{ action: "https://elsewhere.example.test" }, { method: "get" }, { top: false }]) {
  test(`does not fill a rejected form ${JSON.stringify(options)}`, () => {
    const { run, sent, fields } = fixture(options);
    assert.equal(run({ username: "fixture", password: "sentinel" }), false);
    assert.deepEqual(sent, []);
    assert.deepEqual(fields.map(field => field.value), ["", "", ""]);
  });
}


test("fills a masked field named otp only with a one-time code", () => {
  const { run, sent } = fixture({ maskedOtp: true });
  assert.equal(run({ username: "fixture", password: "sentinel" }), false);
  assert.deepEqual(sent, []);
  assert.equal(run({ otp: "123456" }), true);
  assert.deepEqual(sent, [["", "", "123456"]]);
});

test("rejects multiple eligible OTP fields", () => {
  const { run, sent } = fixture({ duplicateOtp: true });
  assert.equal(run({ otp: "123456" }), false);
  assert.deepEqual(sent, []);
});
