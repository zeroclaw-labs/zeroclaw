import { readFileSync } from "node:fs";
import vm from "node:vm";
import assert from "node:assert/strict";
import test from "node:test";

const source = readFileSync(new URL("./browser_credential_form.js", import.meta.url), "utf8");
const origin = "https://login.example.test";

function fixture({ action = origin + "/submit", method = "post", top = true } = {}) {
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
      return [selector.includes("one-time-code") ? fields[2] :
        selector.includes("password") ? fields[1] : fields[0]];
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
