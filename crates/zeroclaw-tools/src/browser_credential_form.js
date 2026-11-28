function submitCatalogCredential({ origin, values, stage = "auto", field = null, frame = null }) {
  if (window.top !== window || window.location.origin !== origin) return false;
  let doc = document;
  try {
    if (frame) {
      const frames = [...doc.querySelectorAll(frame)];
      if (frames.length !== 1 || frames[0].tagName !== "IFRAME" ||
          !frames[0].getClientRects().length || !frames[0].contentDocument) return false;
      doc = frames[0].contentDocument;
      if (new URL(doc.URL).origin !== origin) return false;
    }
    const view = doc.defaultView ?? window;
    const input = view.HTMLInputElement ?? HTMLInputElement;
    const formType = view.HTMLFormElement ?? HTMLFormElement;
    const usable = (element) => element instanceof input && !element.disabled &&
      !element.readOnly && element.getClientRects().length !== 0 &&
      (!view.getComputedStyle || view.getComputedStyle(element).visibility === "visible");
    const select = selector => [...doc.querySelectorAll(selector)].filter(usable);
    const unique = selector => {
      const all = select(selector);
      return all.length === 1 ? all[0] : null;
    };

    const otpFields = 'input[autocomplete~="one-time-code"], input[name="otp"]';
    const passwordFields = 'input[type="password"]:not([autocomplete~="one-time-code"]):not([name="otp"])';
    const usernameFields = 'input[autocomplete~="username"], input[name="username"], input[type="email"]';
    const kind = stage === "auto" ? (values.otp ? "otp" : "login") : stage;
    let fields;
    if (kind === "otp") {
      if (typeof values.otp !== "string") return false;
      const controls = select(field ?? otpFields);
      if (controls.length === 1 && controls[0].matches(otpFields)) fields = [[controls[0], values.otp]];
      else if (controls.length === values.otp.length &&
               controls.every(control => control.maxLength === 1 && (field || control.matches(otpFields)))) {
        fields = controls.map((control, index) => [control, values.otp[index]]);
      } else return false;
      if (fields.some(([control]) => !["text", "password", "tel", "number"].includes(control.type))) return false;
    } else if (kind === "username") {
      const control = unique(field ?? usernameFields);
      if (!control || !["text", "email", "tel"].includes(control.type) ||
          typeof values.username !== "string") return false;
      fields = [[control, values.username]];
    } else if (kind === "password" || kind === "login") {
      const control = unique(field ?? passwordFields);
      if (!control || control.type !== "password" || control.matches?.(otpFields) ||
          typeof values.password !== "string") return false;
      fields = [[control, values.password]];
      if (kind === "login") {
        const username = unique(usernameFields);
        if (!username || username === control || typeof values.username !== "string") return false;
        fields.unshift([username, values.username]);
      }
    } else return false;
    const form = fields[0][0].form;
    if (!form || fields.some(([control]) => control.form !== form)) return false;
    const submitters = [...form.elements].filter(control =>
      ["submit", "image"].includes(control.type) && !control.disabled &&
      control.getClientRects().length !== 0
    );
    if (submitters.length > 1) return false;
    const submitter = submitters[0];
    const action = submitter?.hasAttribute("formaction") ? submitter.formAction : form.action;
    const method = submitter?.hasAttribute("formmethod") ? submitter.formMethod : form.method;
    const target = submitter?.hasAttribute("formtarget") ? submitter.formTarget : form.target;
    if (method.toLowerCase() !== "post" || (target && target !== "_self") ||
        new URL(action, doc.baseURI).origin !== origin) return false;

    let submission;
    const observeSubmit = event => {
      if (event.target === form) submission = event;
    };
    form.addEventListener("submit", observeSubmit, true);
    try {
      for (const [control, value] of fields) control.value = value;
      if (submitter) formType.prototype.requestSubmit.call(form, submitter);
      else formType.prototype.requestSubmit.call(form);
      return submission !== undefined && !submission.defaultPrevented;
    } finally {
      form.removeEventListener("submit", observeSubmit, true);
      for (const [control] of fields) control.value = "";
    }
  } catch {
    return false;
  }
}
