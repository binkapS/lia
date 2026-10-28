"use strict";

/* ------------------------------------------------------------------ */
/* The words, as they are written                                      */
/* ------------------------------------------------------------------ */

function escape(text) {
  return String(text).replace(/[&<>"]/g, (c) => {
    return { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c];
  });
}

function inline(text) {
  let out = escape(text);
  out = out.replace(/`([^`]+)`/g, "<code>$1</code>");
  out = out.replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>");
  out = out.replace(/(^|\s)\*([^*\s][^*]*)\*/g, "$1<em>$2</em>");
  return out;
}

/* A little of the markdown: enough to read a reply. */
function markdown(text) {
  const out = [];
  let fence = null;
  let items = null;

  function flush() {
    if (items) {
      out.push("<ul>" + items.map((i) => "<li>" + i + "</li>").join("") + "</ul>");
      items = null;
    }
  }

  for (const raw of String(text).split("\n")) {
    if (/^\s*(```|~~~)/.test(raw)) {
      if (fence === null) {
        flush();
        fence = [];
      } else {
        out.push("<pre><code>" + escape(fence.join("\n")) + "</code></pre>");
        fence = null;
      }
      continue;
    }
    if (fence !== null) {
      fence.push(raw);
      continue;
    }

    const heading = /^\s*#{1,6}\s+(.*)$/.exec(raw);
    const bullet = /^\s*[-*+]\s+(.*)$/.exec(raw);
    const numbered = /^\s*\d+\.\s+(.*)$/.exec(raw);

    if (heading) {
      flush();
      out.push("<h4>" + inline(heading[1]) + "</h4>");
      continue;
    }
    if (bullet || numbered) {
      if (!items) {
        items = [];
      }
      items.push(inline((bullet || numbered)[1]));
      continue;
    }

    flush();
    if (!raw.trim()) {
      continue;
    }
    out.push("<p>" + inline(raw) + "</p>");
  }

  flush();
  if (fence !== null) {
    out.push("<pre><code>" + escape(fence.join("\n")) + "</code></pre>");
  }
  return out.join("");
}

function pretty(input) {
  if (input === undefined || input === null) {
    return "";
  }
  if (typeof input === "string") {
    return input;
  }
  try {
    return JSON.stringify(input, null, 2);
  } catch (e) {
    return String(input);
  }
}

