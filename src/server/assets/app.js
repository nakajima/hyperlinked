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
