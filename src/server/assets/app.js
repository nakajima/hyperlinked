document.addEventListener(
  "submit",
  (event) => {
    const form = event.target;
    if (!(form instanceof HTMLFormElement)) {
      return;
    }

    const message = form.dataset.confirm;
    if (!message) {
      return;
    }

    if (!window.confirm(message)) {
      event.preventDefault();
      event.stopPropagation();
    }
  },
  true,
);

function trackHyperlinkClick(anchor) {
  const hyperlinkId = anchor.dataset.hyperlinkId;
  if (!hyperlinkId) {
    return;
  }

  const endpoint = `/hyperlinks/${encodeURIComponent(hyperlinkId)}/click`;
  if (navigator.sendBeacon && navigator.sendBeacon(endpoint, "")) {
    return;
  }

  fetch(endpoint, {
    method: "POST",
    credentials: "same-origin",
    keepalive: true,
  }).catch(() => {});
}

document.addEventListener(
  "click",
  (event) => {
    if (!(event.target instanceof Element) || event.button !== 0) {
      return;
    }

    const anchor = event.target.closest("a[data-hyperlink-id]");
    if (!(anchor instanceof HTMLAnchorElement)) {
      return;
    }

    trackHyperlinkClick(anchor);
  },
  true,
);

document.addEventListener(
  "auxclick",
  (event) => {
    if (!(event.target instanceof Element) || event.button !== 1) {
      return;
    }

    const anchor = event.target.closest("a[data-hyperlink-id]");
    if (!(anchor instanceof HTMLAnchorElement)) {
      return;
    }

    trackHyperlinkClick(anchor);
  },
  true,
);

function tokenizeQuery(value) {
  return value
    .split(/\s+/)
    .map((token) => token.trim())
    .filter((token) => token.length > 0);
}

function tokenKey(token) {
  const idx = token.indexOf(":");
  if (idx <= 0) {
    return "";
  }
  const key = token.slice(0, idx).toLowerCase();
  if (key === "kind") {
    return "scope";
  }
  if (key === "is") {
    return "type";
  }
  return key;
}

document.addEventListener(
  "change",
  (event) => {
    if (!(event.target instanceof Element)) {
      return;
    }

    const select = event.target.closest("select[id$='-filter']");
    if (!(select instanceof HTMLSelectElement)) {
      return;
    }

    const suffix = "-filter";
    if (!select.id.endsWith(suffix)) {
      return;
    }

    const key = select.id.slice(0, -suffix.length);
    if (!["status", "scope", "type", "order"].includes(key)) {
      return;
    }

    const form = select.closest("form");
    if (!(form instanceof HTMLFormElement)) {
      return;
    }

    const input = form.querySelector("input[name='q']");
    if (!(input instanceof HTMLInputElement)) {
      return;
    }

    let tokens = tokenizeQuery(input.value);

    tokens = tokens.filter((token) => tokenKey(token) !== key);

    if (select.value) {
      tokens.push(`${key}:${select.value}`);
    }

    input.value = tokens.join(" ");
    form.requestSubmit();
  },
  true,
);
