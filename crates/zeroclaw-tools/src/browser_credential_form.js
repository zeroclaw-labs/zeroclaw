function submitCatalogCredential({ origin, values }) {
  if (window.top !== window || window.location.origin !== origin) return false;
  const usable = (element) => element instanceof HTMLInputElement &&
    !element.disabled && !element.readOnly && element.getClientRects().length !== 0;
  const unique = (selector) => {
    const matches = [...document.querySelectorAll(selector)].filter(usable);
    return matches.length === 1 ? matches[0] : null;
  };
  const otpFields = 'input[autocomplete~="one-time-code"], input[name="otp"]';
  const passwordFields = 'input[type="password"]:not([autocomplete~="one-time-code"]):not([name="otp"])';
  const primary = values.otp ? unique(otpFields) : unique(passwordFields);
  if (!primary || !primary.form) return false;
  const form = primary.form;
  const username = values.otp ? null : unique('input[autocomplete="username"]') ??
    unique('input[type="email"]');
  if (!values.otp && (!username || username.form !== form)) return false;
  if (form.method.toLowerCase() !== "post" ||
      (form.target && form.target !== "_self") ||
      new URL(form.action, document.baseURI).origin !== origin) return false;
  const fields = username
    ? [[username, values.username], [primary, values.password]]
    : [[primary, values.otp]];
  try {
    for (const [field, value] of fields) field.value = value;
    HTMLFormElement.prototype.submit.call(form);
    return true;
  } finally {
    for (const [field] of fields) field.value = "";
  }
}
