import assert from "node:assert/strict";
import test from "node:test";
import React, { act, createElement } from "react";
import { Window } from "happy-dom";
import type { HttpCredentialRow, RemoteFilesGrant } from "../../lib/api.ts";

const dom = new Window({ url: "https://dashboard.example.test/" });
for (const key of ["window", "document", "location", "navigator", "localStorage", "sessionStorage",
  "HTMLElement", "HTMLInputElement", "Node", "Element", "Event", "MouseEvent", "KeyboardEvent"] as const) {
  Object.defineProperty(globalThis, key, {
    configurable: true,
    writable: true,
    value: key === "window" ? dom : dom[key as keyof Window],
  });
}
Object.assign(globalThis, { React, IS_REACT_ACT_ENVIRONMENT: true });

const { createRoot } = await import("react-dom/client");
const { default: CredentialPermissions } = await import("./CredentialPermissions.tsx");

test("unified Add authorization creates, edits and removes a WebDAV grant", async () => {
  const calls: { name: string; operation: string; permissions: { expected_revision: string; remote_files_grants: RemoteFilesGrant[] } }[] = [];
  let revision = 0;
  const row: HttpCredentialRow = {
    name: "REPORT_LOGIN", kind: "user_password", source: "local", origin: "https://files.test",
    permission_revision: "0", http_origins: ["https://api.test"],
    remote_files_resources: [{ resource: "REPORTS", root: "https://files.test/dav/reports/" }],
  };
  globalThis.fetch = async (input, init) => {
    assert.match(String(input), /\/api\/config\/http-credentials$/);
    if (init?.method === "POST") {
      const update = JSON.parse(String(init.body));
      calls.push(update);
      assert.equal(update.permissions.expected_revision, row.permission_revision);
      row.remote_files_grants = update.permissions.remote_files_grants;
      row.permission_revision = String(++revision);
      return new Response(null, { status: 204 });
    }
    return Response.json({ encryption_enabled: true, credentials: [row] });
  };

  const container = document.createElement("main");
  document.body.append(container);
  const root = createRoot(container);
  const button = (label: string) => {
    const element = [...container.querySelectorAll("button")].find(item => item.textContent?.trim() === label);
    assert.ok(element, label);
    return element;
  };
  const checkbox = (label: string) => {
    const field = [...container.querySelectorAll("label")].find(item => item.textContent?.trim() === label);
    const input = field?.querySelector("input");
    assert.ok(input, label);
    return input;
  };

  try {
    await act(async () => root.render(createElement(CredentialPermissions)));
    await act(async () => button("Add authorization").click());
    await act(async () => button("HTTP Request").click());
    const option = [...container.querySelectorAll("[role=option]")].find(item => item.textContent?.includes("Remote Files (WebDAV)"));
    assert.ok(option);
    await act(async () => (option as HTMLElement).click());
    assert.ok(container.textContent?.includes("WebDAV operations"));
    assert.ok(container.textContent?.includes("https://files.test/dav/reports/"));
    await act(async () => checkbox("List directory").click());
    await act(async () => checkbox("Require human approval").click());
    const submit = container.querySelector<HTMLButtonElement>('button[type="submit"]');
    assert.ok(submit);
    await act(async () => submit.click());
    assert.deepEqual(calls[0]?.permissions, {
      expected_revision: "0",
      remote_files_grants: [{
        resource: "REPORTS", root: "https://files.test/dav/reports/",
        rights: { operations: ["list_directory"], require_human_approval: true },
      }],
    });

    const remoteRow = [...container.querySelectorAll("li")].find(item => item.textContent?.includes("REPORTS"));
    assert.ok(remoteRow);
    const controls = remoteRow.querySelectorAll("button");
    assert.equal(controls.length, 2);
    const edit = controls[0];
    assert.ok(edit);
    await act(async () => edit.click());
    assert.equal(checkbox("Require human approval").checked, true);
    await act(async () => checkbox("Get file info").click());
    const save = container.querySelector<HTMLButtonElement>('button[type="submit"]');
    assert.ok(save);
    await act(async () => save.click());
    assert.deepEqual(calls[1]?.permissions.remote_files_grants[0]?.rights.operations, ["list_directory", "get_file_info"]);

    const savedRow = [...container.querySelectorAll("li")].find(item => item.textContent?.includes("REPORTS"));
    const remove = savedRow?.querySelectorAll("button")[1];
    assert.ok(remove);
    await act(async () => remove.click());
    assert.deepEqual(calls[2]?.permissions, { expected_revision: "2", remote_files_grants: [] });
    assert.ok(container.textContent?.includes("https://api.test"));
  } finally {
    await act(async () => root.unmount());
    container.remove();
  }
});
