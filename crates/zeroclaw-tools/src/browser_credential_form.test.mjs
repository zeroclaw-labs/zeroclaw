import { readFileSync } from "node:fs";
import vm from "node:vm";
import assert from "node:assert/strict";
import test from "node:test";

const source = readFileSync(new URL("./browser_credential_form.js", import.meta.url), "utf8");
const origin = "https://login.example.test";

function fixture({ action = origin + "/submit", method = "post", top = true, maskedOtp = false, duplicateOtp = false, frameOrigin = origin } = {}) {
  const sent = [];
  class Form { submit() { sent.push(fields.map(field => field.value)); } }
  class Input {
    value = "";
    getClientRects() { return [1]; }
  }
  const form = Object.assign(new Form(), { action, method, target: "" });
  const fields = ["text", "password", maskedOtp ? "password" : "text"].map(type => Object.assign(new Input(), { form, type }));
  const segments = Array.from({length:6}, () => Object.assign(new Input(), { form, type:"text", maxLength:1 }));
  fields.push(...segments);
  const window = { location: { origin } };
  window.top = top ? window : {};
  const document = {
    baseURI: origin,
    URL: frameOrigin+"/form",
    querySelectorAll(selector) {
      if (selector === "#embedded") return [{tagName:"IFRAME",getClientRects:()=>[1],contentDocument:document}];
      if (selector === ".segment") return segments;

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
  return { run: (values, options) => run({ origin, values, ...options }), sent, fields };
}

test("submits synthetic login and clears input values", () => {
  const { run, sent, fields } = fixture();
  assert.equal(run({ username: "fixture", password: "sentinel" }), true);
  assert.deepEqual(sent, [["fixture", "sentinel", "", "", "", "", "", "", ""]]);
  assert.ok(fields.every(field => field.value === ""));
});

test("submits only the one-time code", () => {
  const { run, sent, fields } = fixture();
  assert.equal(run({ otp: "123456" }), true);
  assert.deepEqual(sent, [["", "", "123456", "", "", "", "", "", ""]]);
  assert.ok(fields.every(field => field.value === ""));
});

for (const options of [{ action: "https://elsewhere.example.test" }, { method: "get" }, { top: false }]) {
  test(`does not fill a rejected form ${JSON.stringify(options)}`, () => {
    const { run, sent, fields } = fixture(options);
    assert.equal(run({ username: "fixture", password: "sentinel" }), false);
    assert.deepEqual(sent, []);
    assert.ok(fields.every(field => field.value === ""));
  });
}


test("fills a masked field named otp only with a one-time code", () => {
  const { run, sent } = fixture({ maskedOtp: true });
  assert.equal(run({ username: "fixture", password: "sentinel" }), false);
  assert.deepEqual(sent, []);
  assert.equal(run({ otp: "123456" }), true);
  assert.deepEqual(sent, [["", "", "123456", "", "", "", "", "", ""]]);
});

test("rejects multiple eligible OTP fields", () => {
  const { run, sent } = fixture({ duplicateOtp: true });
  assert.equal(run({ otp: "123456" }), false);
  assert.deepEqual(sent, []);
});


test("submits username and password in independent stages", () => {
  const { run, sent } = fixture();
  assert.equal(run({ username: "fixture" }, {stage:"username"}), true);
  assert.equal(run({ password: "sentinel" }, {stage:"password"}), true);
  assert.deepEqual(sent.map(row=>row.slice(0,2)), [["fixture",""],["","sentinel"]]);
});

test("submits OTP into a same-origin iframe and refuses another origin", () => {
  const allowed=fixture();
  assert.equal(allowed.run({otp:"123456"},{frame:"#embedded"}), true);
  const denied=fixture({frameOrigin:"https://elsewhere.example.test"});
  assert.equal(denied.run({otp:"123456"},{frame:"#embedded"}), false);
  assert.deepEqual(denied.sent, []);
});

test("fills a selected six-segment OTP group", () => {
  const {run,sent,fields}=fixture();
  assert.equal(run({otp:"123456"},{stage:"otp",field:".segment"}),true);
  assert.deepEqual(sent[0].slice(3),["1","2","3","4","5","6"]);
  assert.ok(fields.every(field=>field.value===""));
});
