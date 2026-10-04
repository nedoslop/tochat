// In-app replacements for window.alert / confirm / prompt.
//
// Native browser modals look out of place inside a Tauri webview and can't
// be styled. These functions render a modal into #modal-root and return
// promises that resolve when the user dismisses it.

function buildOverlay() {
  const overlay = document.createElement("div");
  overlay.className = "modal-overlay";
  return overlay;
}

function focusFirst(root, selector) {
  const el = root.querySelector(selector);
  if (el && typeof el.focus === "function") {
    requestAnimationFrame(() => el.focus());
  }
}

/**
 * Generic modal.
 *
 * - `buttons`: array of { label, value, primary, danger, submit }.
 *   The button with `submit` (or `primary`) is triggered by Enter.
 * - `getSubmitValue(modal)`: optional — overrides the value returned when
 *   the form is submitted. Used by showPrompt to read the input.
 *
 * Resolves with the chosen button's `value` (or `null` on dismiss).
 */
function showModal({ title, message, input, buttons, getSubmitValue }) {
  return new Promise((resolve) => {
    const overlay = buildOverlay();
    const modal = document.createElement("form");
    modal.className = "modal";
    modal.setAttribute("novalidate", "");

    const titleEl = document.createElement("div");
    titleEl.className = "modal-title";
    titleEl.textContent = title;
    modal.appendChild(titleEl);

    if (message) {
      const msgEl = document.createElement("div");
      msgEl.className = "modal-message";
      msgEl.textContent = message;
      modal.appendChild(msgEl);
    }

    let inputEl = null;
    if (input) {
      inputEl = document.createElement("input");
      inputEl.type = input.type ?? "text";
      inputEl.className = "modal-input";
      inputEl.value = input.value ?? "";
      inputEl.placeholder = input.placeholder ?? "";
      inputEl.spellcheck = false;
      inputEl.autocomplete = "off";
      modal.appendChild(inputEl);
    }

    const actions = document.createElement("div");
    actions.className = "modal-actions";

    let resolved = false;
    const finish = (value) => {
      if (resolved) return;
      resolved = true;
      document.removeEventListener("keydown", onKey);
      overlay.remove();
      resolve(value);
    };

    for (const b of buttons) {
      const btn = document.createElement("button");
      btn.type = b.submit ? "submit" : "button";
      btn.className =
        "btn " +
        (b.danger
          ? "btn-danger-solid"
          : b.primary
          ? "btn-primary"
          : "btn-ghost");
      btn.textContent = b.label;
      if (!b.submit) {
        btn.addEventListener("click", () => finish(b.value));
      }
      actions.appendChild(btn);
    }
    modal.appendChild(actions);

    modal.addEventListener("submit", (e) => {
      e.preventDefault();
      const primary = buttons.find((b) => b.submit || b.primary);
      const value = getSubmitValue
        ? getSubmitValue(modal)
        : primary
        ? primary.value
        : null;
      finish(value);
    });

    overlay.addEventListener("mousedown", (e) => {
      if (e.target === overlay) finish(null);
    });

    const onKey = (e) => {
      if (e.key === "Escape") {
        e.preventDefault();
        finish(null);
      }
    };
    document.addEventListener("keydown", onKey);

    overlay.appendChild(modal);
    document.getElementById("modal-root").appendChild(overlay);

    focusFirst(modal, inputEl ? ".modal-input" : ".btn-primary, .btn-danger-solid");
  });
}

/** Replacement for window.alert. Resolves when dismissed. */
export function showAlert(message, { title = "Notice", okText = "OK" } = {}) {
  return showModal({
    title,
    message,
    buttons: [{ label: okText, value: true, primary: true, submit: true }],
  }).then(() => undefined);
}

/** Replacement for window.confirm. Resolves true/false. */
export function showConfirm(
  message,
  { title = "Confirm", okText = "OK", cancelText = "Cancel", danger = false } = {},
) {
  return showModal({
    title,
    message,
    buttons: [
      { label: cancelText, value: false },
      { label: okText, value: true, primary: !danger, danger, submit: true },
    ],
  }).then((v) => v === true);
}

/**
 * Replacement for window.prompt. Resolves the entered string on OK,
 * or null if cancelled / dismissed. An empty string is a valid result —
 * callers must distinguish `null` (cancelled) from `""` (submitted empty).
 */
export function showPrompt(
  message,
  {
    title = "Input",
    defaultValue = "",
    placeholder = "",
    okText = "OK",
    cancelText = "Cancel",
    danger = false,
  } = {},
) {
  return showModal({
    title,
    message,
    input: { value: defaultValue, placeholder },
    buttons: [
      { label: cancelText, value: null },
      { label: okText, value: null, primary: !danger, danger, submit: true },
    ],
    getSubmitValue: (modal) =>
      modal.querySelector(".modal-input")?.value ?? "",
  });
}