"use strict";

const inboxUi = {
  overlay: document.getElementById("inbox-overlay"),
  open: document.getElementById("inbox-open"),
  close: document.getElementById("inbox-close"),
  form: document.getElementById("inbox-capture"),
  text: document.getElementById("inbox-text"),
  kind: document.getElementById("inbox-kind"),
  priority: document.getElementById("inbox-priority"),
  archived: document.getElementById("inbox-archived"),
  count: document.getElementById("inbox-count"),
  status: document.getElementById("inbox-status"),
  list: document.getElementById("inbox-list"),
};

async function inboxRequest(path, options = {}) {
  const headers = { ...apiHeaders(), ...(options.headers || {}) };
  if (options.body) headers["content-type"] = "application/json";
  const response = await fetch(path, { ...options, headers });
  if (!response.ok) {
    let message = `inbox request failed (${response.status})`;
    try {
      const body = await response.json();
      if (body.error) message = body.error;
    } catch (e) {
      /* Keep the status message for an empty or non-JSON response. */
    }
    throw new Error(message);
  }
  return response.status === 204 ? null : response.json();
}

function inboxButton(label, action, id) {
  const button = document.createElement("button");
  button.type = "button";
  button.textContent = label;
  button.dataset.action = action;
  button.dataset.id = id;
  return button;
}

function renderInbox(records) {
  inboxUi.list.replaceChildren();
  inboxUi.count.textContent = `${records.length} record${records.length === 1 ? "" : "s"}`;
  if (!records.length) {
    const empty = document.createElement("li");
    empty.className = "inbox-empty";
    empty.textContent = "Your inbox is clear.";
    inboxUi.list.append(empty);
    return;
  }
  for (const record of records) {
    const item = document.createElement("li");
    item.className = "inbox-item";
    item.dataset.state = record.state;

    const head = document.createElement("div");
    head.className = "inbox-item-head";
    const kind = document.createElement("span");
    kind.textContent = `${record.kind} · ${record.priority}`;
    const state = document.createElement("span");
    state.textContent = record.state;
    head.append(kind, state);

    const text = document.createElement("div");
    text.className = "inbox-item-text";
    text.textContent = record.text;
    item.append(head, text);
    if (record.snoozed_until) {
      const deferred = document.createElement("div");
      deferred.className = "inbox-item-meta";
      deferred.textContent = `deferred until ${new Date(record.snoozed_until).toLocaleString()}`;
      item.append(deferred);
    }

    const actions = document.createElement("div");
    actions.className = "inbox-actions";
    if (record.state === "open") actions.append(inboxButton("complete", "complete", record.id));
    if (record.state !== "archived") actions.append(inboxButton("archive", "archive", record.id));
    actions.append(inboxButton("delete", "delete", record.id));
    item.append(actions);

    if (record.state === "open") {
      const defer = document.createElement("form");
      defer.className = "inbox-defer";
      defer.dataset.id = record.id;
      const when = document.createElement("input");
      when.type = "datetime-local";
      when.setAttribute("aria-label", "defer until");
      const submit = document.createElement("button");
      submit.type = "submit";
      submit.textContent = "defer";
      defer.append(when, submit);
      item.append(defer);
    }
    inboxUi.list.append(item);
  }
}

async function loadInbox() {
  inboxUi.status.textContent = "";
  try {
    const archived = inboxUi.archived.checked ? "?include_archived=true" : "";
    renderInbox(await inboxRequest(`/v1/inbox${archived}`));
  } catch (error) {
    inboxUi.status.textContent = error.message;
    inboxUi.count.textContent = "unavailable";
  }
}

inboxUi.open.addEventListener("click", async () => {
  inboxUi.overlay.hidden = false;
  await loadInbox();
  inboxUi.text.focus();
});
inboxUi.close.addEventListener("click", () => { inboxUi.overlay.hidden = true; });
inboxUi.overlay.addEventListener("click", (event) => {
  if (event.target === inboxUi.overlay) inboxUi.overlay.hidden = true;
});
document.addEventListener("keydown", (event) => {
  if (event.key === "Escape" && !inboxUi.overlay.hidden) inboxUi.overlay.hidden = true;
});
inboxUi.archived.addEventListener("change", loadInbox);
inboxUi.form.addEventListener("submit", async (event) => {
  event.preventDefault();
  inboxUi.status.textContent = "";
  try {
    await inboxRequest("/v1/inbox", {
      method: "POST",
      body: JSON.stringify({
        text: inboxUi.text.value,
        kind: inboxUi.kind.value,
        priority: inboxUi.priority.value,
      }),
    });
    inboxUi.text.value = "";
    await loadInbox();
    inboxUi.text.focus();
  } catch (error) {
    inboxUi.status.textContent = error.message;
  }
});
inboxUi.list.addEventListener("click", async (event) => {
  const button = event.target.closest("button[data-action]");
  if (!button) return;
  if (button.dataset.action === "delete" && !window.confirm("Delete this inbox record permanently?")) return;
  const state = button.dataset.action === "complete" ? "done" : "archived";
  try {
    if (button.dataset.action === "delete") {
      await inboxRequest(`/v1/inbox/${encodeURIComponent(button.dataset.id)}`, { method: "DELETE" });
    } else {
      await inboxRequest(`/v1/inbox/${encodeURIComponent(button.dataset.id)}`, {
        method: "PATCH",
        body: JSON.stringify({ state }),
      });
    }
    await loadInbox();
  } catch (error) {
    inboxUi.status.textContent = error.message;
  }
});
inboxUi.list.addEventListener("submit", async (event) => {
  const form = event.target.closest(".inbox-defer");
  if (!form) return;
  event.preventDefault();
  const input = form.querySelector("input");
  if (!input.value) {
    inboxUi.status.textContent = "Choose a time to defer this record.";
    return;
  }
  try {
    await inboxRequest(`/v1/inbox/${encodeURIComponent(form.dataset.id)}/defer`, {
      method: "POST",
      body: JSON.stringify({ until: new Date(input.value).toISOString() }),
    });
    await loadInbox();
  } catch (error) {
    inboxUi.status.textContent = error.message;
  }
});
