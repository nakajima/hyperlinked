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

function tokenValue(token) {
  const idx = token.indexOf(":");
  if (idx <= 0) {
    return "";
  }

  return token
    .slice(idx + 1)
    .toLowerCase()
    .replace(/_/g, "-");
}

function isDiscoveredScopeToken(token) {
  const key = tokenKey(token);
  if (key === "scope") {
    return true;
  }

  return key === "with" && tokenValue(token) === "discovered";
}

function parseStandaloneHttpUrl(value) {
  const trimmed = value.trim();
  if (!trimmed || /\s/.test(trimmed)) {
    return null;
  }

  const candidates = [trimmed];
  if (!trimmed.includes("://")) {
    const lower = trimmed.toLowerCase();
    const looksLikeHostOrPath =
      lower.startsWith("localhost") || /[./:?#]/.test(trimmed);
    if (looksLikeHostOrPath) {
      candidates.push(`https://${trimmed}`);
    }
  }

  for (const candidate of candidates) {
    let parsed;
    try {
      parsed = new URL(candidate);
    } catch (_) {
      continue;
    }

    if (parsed.protocol === "http:" || parsed.protocol === "https:") {
      return candidate;
    }
  }

  return null;
}

const INLINE_REVEAL_ENTER_MS = 240;
const inlineRevealEnterTimers = new WeakMap();

function setInlineRevealVisible(slot, button, visible) {
  const wasVisible = slot.dataset.visible === "true";
  const activeTimer = inlineRevealEnterTimers.get(slot);
  if (activeTimer) {
    window.clearTimeout(activeTimer);
    inlineRevealEnterTimers.delete(slot);
  }

  if (!visible) {
    slot.dataset.entering = "false";
    slot.dataset.visible = "false";
    slot.setAttribute("aria-hidden", "true");
    button.disabled = true;
    return;
  }

  if (wasVisible) {
    slot.dataset.visible = "true";
    slot.setAttribute("aria-hidden", "false");
    button.disabled = false;
    return;
  }

  slot.dataset.entering = "true";
  slot.dataset.visible = visible ? "true" : "false";
  slot.setAttribute("aria-hidden", "false");
  button.disabled = false;

  const timerId = window.setTimeout(() => {
    slot.dataset.entering = "false";
    inlineRevealEnterTimers.delete(slot);
  }, INLINE_REVEAL_ENTER_MS + 40);
  inlineRevealEnterTimers.set(slot, timerId);
}

function hideUrlIntent(container, addSlot, addButton, rootMessage, addUrlInput) {
  container.classList.add("hidden");
  container.setAttribute("aria-hidden", "true");
  setInlineRevealVisible(addSlot, addButton, false);
  rootMessage.hidden = true;
  rootMessage.classList.add("hidden");
  addUrlInput.value = "";
}

function showUrlIntentAdd(
  container,
  addSlot,
  addButton,
  rootMessage,
  addUrlInput,
  canonicalUrl,
) {
  container.classList.add("hidden");
  container.setAttribute("aria-hidden", "true");
  setInlineRevealVisible(addSlot, addButton, true);
  rootMessage.hidden = true;
  rootMessage.classList.add("hidden");
  addUrlInput.value = canonicalUrl;
}

function showUrlIntentRootMessage(
  container,
  addSlot,
  addButton,
  rootMessage,
  addUrlInput,
  canonicalUrl,
) {
  container.classList.remove("hidden");
  container.setAttribute("aria-hidden", "false");
  setInlineRevealVisible(addSlot, addButton, false);
  rootMessage.hidden = false;
  rootMessage.classList.remove("hidden");
  addUrlInput.value = canonicalUrl;
}

function initializeUrlIntent() {
  const queryInput = document.querySelector("[data-url-intent-input]");
  if (!(queryInput instanceof HTMLInputElement)) {
    return;
  }

  const container = document.querySelector("[data-url-intent]");
  const addForm = document.querySelector("[data-url-intent-add-form]");
  const addSlot = document.querySelector("[data-url-intent-add-slot]");
  const addUrlInput = document.querySelector("[data-url-intent-add-url]");
  const addButton = document.querySelector("[data-url-intent-add-button]");
  const rootMessage = document.querySelector("[data-url-intent-root-message]");
  if (
    !(container instanceof HTMLElement) ||
    !(addForm instanceof HTMLFormElement) ||
    !(addSlot instanceof HTMLElement) ||
    !(addUrlInput instanceof HTMLInputElement) ||
    !(addButton instanceof HTMLButtonElement) ||
    !(rootMessage instanceof HTMLElement)
  ) {
    return;
  }

  let lookupTimer = null;
  let activeLookupController = null;
  let latestLookupRequestId = 0;

  const syncAddSlotSize = () => {
    const buttonWidth = addButton.offsetWidth;
    if (buttonWidth > 0) {
      addSlot.style.setProperty("--inline-reveal-target-size", `${buttonWidth}px`);
    }
  };

  syncAddSlotSize();
  window.addEventListener("resize", syncAddSlotSize);

  const cancelLookup = () => {
    if (lookupTimer !== null) {
      clearTimeout(lookupTimer);
      lookupTimer = null;
    }

    if (activeLookupController) {
      activeLookupController.abort();
      activeLookupController = null;
    }
  };

  const runLookup = async (candidate) => {
    if (activeLookupController) {
      activeLookupController.abort();
    }

    const requestId = ++latestLookupRequestId;
    const controller = new AbortController();
    activeLookupController = controller;

    try {
      const response = await fetch(
        `/hyperlinks/lookup?url=${encodeURIComponent(candidate)}`,
        {
          method: "GET",
          credentials: "same-origin",
          cache: "no-store",
          signal: controller.signal,
        },
      );
      if (!response.ok) {
        if (requestId === latestLookupRequestId) {
          showUrlIntentAdd(
            container,
            addSlot,
            addButton,
            rootMessage,
            addUrlInput,
            candidate,
          );
        }
        return;
      }

      const data = await response.json();
      if (requestId !== latestLookupRequestId) {
        return;
      }

      const latestCandidate = parseStandaloneHttpUrl(queryInput.value);
      if (!latestCandidate || latestCandidate !== candidate) {
        return;
      }

      const status = typeof data?.status === "string" ? data.status : "invalid_url";
      const canonicalUrl = typeof data?.canonical_url === "string" ? data.canonical_url : "";

      if ((status === "not_found" || status === "discovered") && canonicalUrl) {
        showUrlIntentAdd(
          container,
          addSlot,
          addButton,
          rootMessage,
          addUrlInput,
          canonicalUrl,
        );
        return;
      }

      if (status === "root") {
        showUrlIntentRootMessage(
          container,
          addSlot,
          addButton,
          rootMessage,
          addUrlInput,
          canonicalUrl,
        );
        return;
      }

      if (canonicalUrl) {
        showUrlIntentAdd(
          container,
          addSlot,
          addButton,
          rootMessage,
          addUrlInput,
          canonicalUrl,
        );
        return;
      }

      hideUrlIntent(container, addSlot, addButton, rootMessage, addUrlInput);
    } catch (error) {
      if (error instanceof DOMException && error.name === "AbortError") {
        return;
      }

      if (requestId === latestLookupRequestId) {
        showUrlIntentAdd(
          container,
          addSlot,
          addButton,
          rootMessage,
          addUrlInput,
          candidate,
        );
      }
    } finally {
      if (activeLookupController === controller) {
        activeLookupController = null;
      }
    }
  };

  const scheduleLookup = () => {
    const candidate = parseStandaloneHttpUrl(queryInput.value);
    if (!candidate) {
      cancelLookup();
      hideUrlIntent(container, addSlot, addButton, rootMessage, addUrlInput);
      return;
    }

    // Show the add affordance immediately, then refine with lookup results.
    showUrlIntentAdd(container, addSlot, addButton, rootMessage, addUrlInput, candidate);

    if (lookupTimer !== null) {
      clearTimeout(lookupTimer);
    }

    lookupTimer = setTimeout(() => {
      lookupTimer = null;
      void runLookup(candidate);
    }, 200);
  };

  queryInput.addEventListener("input", scheduleLookup);
  queryInput.addEventListener("change", scheduleLookup);

  addForm.addEventListener("submit", () => {
    if (!addButton.disabled) {
      addButton.disabled = true;
    }
  });

  scheduleLookup();
}

document.addEventListener(
  "change",
  (event) => {
    if (!(event.target instanceof Element)) {
      return;
    }

    const select = event.target.closest("select[data-filter-key]");
    if (!(select instanceof HTMLSelectElement)) {
      return;
    }

    const key = select.dataset.filterKey || "";
    if (!["status", "type", "order"].includes(key)) {
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

document.addEventListener(
  "change",
  (event) => {
    if (!(event.target instanceof Element)) {
      return;
    }

    const checkbox = event.target.closest("input[data-discovered-filter]");
    if (!(checkbox instanceof HTMLInputElement) || checkbox.type !== "checkbox") {
      return;
    }

    const form = checkbox.closest("form");
    if (!(form instanceof HTMLFormElement)) {
      return;
    }

    const input = form.querySelector("input[name='q']");
    if (!(input instanceof HTMLInputElement)) {
      return;
    }

    let tokens = tokenizeQuery(input.value);
    tokens = tokens.filter((token) => !isDiscoveredScopeToken(token));

    if (checkbox.checked) {
      tokens.push("with:discovered");
    }

    input.value = tokens.join(" ");
    form.requestSubmit();
  },
  true,
);

initializeUrlIntent();

function updateQueuePendingBadge(pending) {
  const badge = document.querySelector("[data-queue-pending-badge]");
  if (!(badge instanceof HTMLElement)) {
    return;
  }

  if (!Number.isFinite(pending) || pending <= 0) {
    badge.classList.add("hidden");
    badge.textContent = "0";
    return;
  }

  badge.classList.remove("hidden");
  badge.textContent = String(pending);
}

async function refreshQueuePendingBadge() {
  try {
    const response = await fetch("/admin/jobs/pending-count", {
      method: "GET",
      credentials: "same-origin",
      cache: "no-store",
    });
    if (!response.ok) {
      return;
    }

    const data = await response.json();
    const pending = Number(data?.pending);
    updateQueuePendingBadge(pending);
  } catch (_) {}
}

if (document.querySelector("[data-queue-pending-badge]")) {
  refreshQueuePendingBadge();
  setInterval(() => {
    if (document.visibilityState !== "visible") {
      return;
    }
    refreshQueuePendingBadge();
  }, 5000);

  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible") {
      refreshQueuePendingBadge();
    }
  });
}
