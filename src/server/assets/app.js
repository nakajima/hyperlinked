document.addEventListener(
  "submit",
  (event) => {
    const form = event.target;
    if (!(form instanceof HTMLFormElement)) {
      return;
    }

    const submitter = "submitter" in event ? event.submitter : null;
    const message =
      (submitter instanceof HTMLElement ? submitter.dataset.confirm : null) ||
      form.dataset.confirm;
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
    button.dataset.entering = "false";
    button.dataset.visible = "false";
    slot.setAttribute("aria-hidden", "true");
    button.disabled = true;
    return;
  }

  if (wasVisible) {
    slot.dataset.visible = "true";
    button.dataset.visible = "true";
    slot.setAttribute("aria-hidden", "false");
    button.disabled = false;
    return;
  }

  slot.dataset.entering = "true";
  slot.dataset.visible = "true";
  button.dataset.entering = "true";
  button.dataset.visible = "true";
  slot.setAttribute("aria-hidden", "false");
  button.disabled = false;

  const timerId = window.setTimeout(() => {
    slot.dataset.entering = "false";
    button.dataset.entering = "false";
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

function initializeLlmModelDiscovery() {
  const form = document.querySelector("[data-llm-settings-form]");
  if (!(form instanceof HTMLFormElement)) {
    return;
  }

  const baseUrlInput = form.querySelector("[data-llm-base-url]");
  const modelSelect = form.querySelector("[data-llm-model-select]");
  const apiKeyInput = form.querySelector("[data-llm-api-key]");
  const authHeaderNameInput = form.querySelector("[data-llm-auth-header-name]");
  const authHeaderPrefixInput = form.querySelector("[data-llm-auth-header-prefix]");
  const backendKindInput = form.querySelector("[data-llm-backend-kind]");
  const checkButton = form.querySelector("[data-llm-check-button]");
  const modelStatus = form.querySelector("[data-llm-model-status]");

  if (
    !(baseUrlInput instanceof HTMLInputElement) ||
    !(modelSelect instanceof HTMLSelectElement) ||
    !(modelStatus instanceof HTMLElement)
  ) {
    return;
  }

  let lookupTimer = null;
  let activeController = null;
  let latestLookupRequestId = 0;
  let lastLookupKey = "";

  const trimmedOrEmpty = (value) => (typeof value === "string" ? value.trim() : "");
  const readInputValue = (input) =>
    input instanceof HTMLInputElement ? trimmedOrEmpty(input.value) : "";
  const backendKindLabel = (kind) => {
    switch (kind) {
      case "ollama":
      case "ollama_api":
        return "Ollama";
      case "openai_compatible":
      case "openai_v1":
        return "OpenAI-compatible";
      default:
        return "Unknown";
    }
  };
  const updateBackendKind = (kind) => {
    if (backendKindInput instanceof HTMLInputElement) {
      backendKindInput.value = trimmedOrEmpty(kind) || "unknown";
    }
  };
  const selectedModel = () => {
    const value = trimmedOrEmpty(modelSelect.value);
    if (value) {
      return value;
    }
    const selected = modelSelect.selectedOptions.item(0);
    return selected ? trimmedOrEmpty(selected.value || selected.textContent || "") : "";
  };
  const setStatus = (text) => {
    modelStatus.textContent = text;
  };
  const applyModelOptions = (models, preferredModel) => {
    const options = [];
    const seen = new Set();
    for (const model of models) {
      const normalized = trimmedOrEmpty(model);
      if (!normalized) {
        continue;
      }
      const key = normalized.toLowerCase();
      if (seen.has(key)) {
        continue;
      }
      seen.add(key);
      options.push(normalized);
    }

    const preferred = trimmedOrEmpty(preferredModel);
    if (preferred && !seen.has(preferred.toLowerCase())) {
      options.unshift(preferred);
    }

    modelSelect.innerHTML = "";
    if (options.length === 0) {
      const placeholder = document.createElement("option");
      placeholder.value = preferred;
      placeholder.textContent = preferred || "No models found";
      modelSelect.append(placeholder);
      modelSelect.disabled = !preferred;
      if (preferred) {
        modelSelect.value = preferred;
      }
      return;
    }

    for (const model of options) {
      const option = document.createElement("option");
      option.value = model;
      option.textContent = model;
      modelSelect.append(option);
    }

    modelSelect.disabled = false;
    if (preferred) {
      modelSelect.value = preferred;
    } else {
      modelSelect.selectedIndex = 0;
    }
  };
  const buildRequestPayload = () => ({
    base_url: trimmedOrEmpty(baseUrlInput.value),
    api_key: readInputValue(apiKeyInput),
    auth_header_name: readInputValue(authHeaderNameInput),
    auth_header_prefix: readInputValue(authHeaderPrefixInput),
  });
  const buildCheckPayload = () => ({
    ...buildRequestPayload(),
    model: selectedModel(),
    backend_kind: readInputValue(backendKindInput),
  });
  const cancelLookup = () => {
    if (lookupTimer !== null) {
      window.clearTimeout(lookupTimer);
      lookupTimer = null;
    }
    if (activeController) {
      activeController.abort();
      activeController = null;
    }
  };

  const runLookup = async () => {
    const requestPayload = buildRequestPayload();
    if (!requestPayload.base_url) {
      lastLookupKey = "";
      cancelLookup();
      updateBackendKind("unknown");
      setStatus("Model options load from the configured endpoint.");
      return;
    }

    const requestKey = JSON.stringify(requestPayload);
    if (requestKey === lastLookupKey) {
      return;
    }
    lastLookupKey = requestKey;

    if (activeController) {
      activeController.abort();
    }

    const lookupRequestId = ++latestLookupRequestId;
    const controller = new AbortController();
    activeController = controller;
    const previousSelection = selectedModel();
    setStatus("Loading model options...");

    try {
      const response = await fetch("/admin/llm-models", {
        method: "POST",
        credentials: "same-origin",
        headers: {
          "content-type": "application/json",
        },
        body: JSON.stringify(requestPayload),
        signal: controller.signal,
      });

      let payload = null;
      try {
        payload = await response.json();
      } catch (_) {}

      if (lookupRequestId !== latestLookupRequestId) {
        return;
      }

      if (!response.ok) {
        const errorMessage =
          typeof payload?.error === "string" && payload.error.trim().length > 0
            ? payload.error.trim()
            : `Model lookup failed (${response.status}).`;
        setStatus(errorMessage);
        return;
      }

      const models = Array.isArray(payload?.models) ? payload.models : [];
      const backendKind = trimmedOrEmpty(payload?.backend_kind);
      applyModelOptions(models, previousSelection);
      updateBackendKind(backendKind);
      setStatus(
        models.length > 0
          ? `${models.length} model${models.length === 1 ? "" : "s"} found (${backendKindLabel(backendKind)}).`
          : `No models found (${backendKindLabel(backendKind)}).`,
      );
    } catch (error) {
      if (controller.signal.aborted || lookupRequestId !== latestLookupRequestId) {
        return;
      }
      setStatus(error instanceof Error ? error.message : "Model lookup failed.");
    } finally {
      if (activeController === controller) {
        activeController = null;
      }
    }
  };

  const runCheck = async () => {
    const requestPayload = buildCheckPayload();
    if (!requestPayload.base_url) {
      setStatus("Enter a base URL before running check.");
      return;
    }
    if (!requestPayload.model) {
      setStatus("Choose a model before running check.");
      return;
    }

    if (checkButton instanceof HTMLButtonElement) {
      checkButton.disabled = true;
    }
    setStatus("Checking endpoint...");

    try {
      const response = await fetch("/admin/llm-check", {
        method: "POST",
        credentials: "same-origin",
        headers: {
          "content-type": "application/json",
        },
        body: JSON.stringify(requestPayload),
      });

      let payload = null;
      try {
        payload = await response.json();
      } catch (_) {}

      if (!response.ok) {
        const errorMessage =
          typeof payload?.error === "string" && payload.error.trim().length > 0
            ? payload.error.trim()
            : `Check failed (${response.status}).`;
        setStatus(errorMessage);
        return;
      }

      const backendKind = trimmedOrEmpty(payload?.backend_kind);
      updateBackendKind(backendKind);
      setStatus(
        typeof payload?.message === "string" && payload.message.trim().length > 0
          ? payload.message.trim()
          : `Check succeeded (${backendKindLabel(backendKind)}).`,
      );
    } catch (_) {
      setStatus("Could not run check request.");
    } finally {
      if (checkButton instanceof HTMLButtonElement) {
        checkButton.disabled = false;
      }
    }
  };

  const scheduleLookup = () => {
    if (lookupTimer !== null) {
      window.clearTimeout(lookupTimer);
    }
    lookupTimer = window.setTimeout(() => {
      lookupTimer = null;
      void runLookup();
    }, 250);
  };

  const watchInput = (input) => {
    if (!(input instanceof HTMLInputElement)) {
      return;
    }
    input.addEventListener("input", scheduleLookup);
    input.addEventListener("change", scheduleLookup);
  };

  watchInput(baseUrlInput);
  watchInput(apiKeyInput);
  watchInput(authHeaderNameInput);
  watchInput(authHeaderPrefixInput);
  if (checkButton instanceof HTMLButtonElement) {
    checkButton.addEventListener("click", () => {
      void runCheck();
    });
  }

  void runLookup();
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

function initializePDFUpload() {
  const root = document.querySelector("[data-pdf-upload]");
  if (!(root instanceof HTMLElement)) {
    return;
  }

  const form = root.querySelector("[data-pdf-upload-form]");
  const titleInput = root.querySelector("[data-pdf-upload-title]");
  const dropzone = root.querySelector("[data-pdf-upload-dropzone]");
  const filenameText = root.querySelector("[data-pdf-upload-filename]");
  const statusText = root.querySelector("[data-pdf-upload-status]");
  const chooseButton = root.querySelector("[data-pdf-upload-choose]");
  const submitButton = root.querySelector("[data-pdf-upload-submit]");
  const fileInput = root.querySelector("[data-pdf-upload-input]");
  const overlay = root.querySelector("[data-pdf-upload-overlay]");
  const resultsWrapper = root.querySelector("[data-pdf-upload-results-wrapper]");
  const resultsList = root.querySelector("[data-pdf-upload-results]");
  const resultTemplate = root.querySelector("[data-pdf-upload-result-template]");

  if (
    !(form instanceof HTMLFormElement) ||
    !(titleInput instanceof HTMLInputElement) ||
    !(dropzone instanceof HTMLElement) ||
    !(filenameText instanceof HTMLElement) ||
    !(statusText instanceof HTMLElement) ||
    !(chooseButton instanceof HTMLButtonElement) ||
    !(submitButton instanceof HTMLButtonElement) ||
    !(fileInput instanceof HTMLInputElement) ||
    !(overlay instanceof HTMLElement) ||
    !(resultsWrapper instanceof HTMLElement) ||
    !(resultsList instanceof HTMLElement) ||
    !(resultTemplate instanceof HTMLTemplateElement)
  ) {
    return;
  }

  const defaultStatusMessage = "Drop one or more PDFs anywhere in this window to upload them right away.";
  const defaultFilenameMessage = "No PDFs selected";
  const MAX_PARALLEL_PDF_UPLOADS = 3;
  let selectedFiles = [];
  let resultItems = [];
  let uploadInFlight = false;
  let windowDragDepth = 0;
  let nextResultItemID = 1;
  let pendingNavigationTimer = null;

  function setStatus(message, tone = "idle") {
    statusText.textContent = message;
    statusText.dataset.tone = tone;
  }

  function cancelPendingNavigation() {
    if (pendingNavigationTimer !== null) {
      window.clearTimeout(pendingNavigationTimer);
      pendingNavigationTimer = null;
    }
  }

  function scheduleNavigation(callback, delayMs) {
    cancelPendingNavigation();
    pendingNavigationTimer = window.setTimeout(() => {
      pendingNavigationTimer = null;
      callback();
    }, delayMs);
  }

  function describeSelectedFiles(files) {
    if (!Array.isArray(files) || files.length === 0) {
      return defaultFilenameMessage;
    }

    if (files.length === 1) {
      return files[0]?.name?.trim() || "document.pdf";
    }

    const firstName = files[0]?.name?.trim();
    if (firstName) {
      return `${firstName} + ${files.length - 1} more PDF${files.length === 2 ? "" : "s"}`;
    }

    return `${files.length} PDFs selected`;
  }

  function setSelectedFiles(files) {
    selectedFiles = Array.isArray(files)
      ? files.filter((file) => file instanceof File)
      : [];
    filenameText.textContent = describeSelectedFiles(selectedFiles);
    submitButton.disabled = uploadInFlight || selectedFiles.length === 0;
  }

  function badgeLabelForState(state) {
    switch (state) {
      case "uploading":
        return "Uploading";
      case "success":
        return "Uploaded";
      case "error":
        return "Failed";
      default:
        return "Ready";
    }
  }

  function renderResults() {
    if (resultItems.length === 0) {
      resultsList.replaceChildren();
      resultsWrapper.hidden = true;
      resultsWrapper.classList.add("hidden");
      return;
    }

    const nodes = [];
    for (const item of resultItems) {
      const row = resultTemplate.content.firstElementChild?.cloneNode(true);
      if (!(row instanceof HTMLElement)) {
        continue;
      }

      const nameElement = row.querySelector("[data-pdf-upload-result-name]");
      const messageElement = row.querySelector("[data-pdf-upload-result-message]");
      const badgeElement = row.querySelector("[data-pdf-upload-result-badge]");
      const linkElement = row.querySelector("[data-pdf-upload-result-link]");

      if (nameElement instanceof HTMLElement) {
        const fileName = item.file?.name?.trim();
        nameElement.textContent = fileName || "document.pdf";
      }

      if (messageElement instanceof HTMLElement) {
        messageElement.textContent = item.message || "";
      }

      if (badgeElement instanceof HTMLElement) {
        badgeElement.dataset.state = item.state || "selected";
        badgeElement.textContent = badgeLabelForState(item.state);
      }

      if (linkElement instanceof HTMLAnchorElement) {
        if (Number.isFinite(item.hyperlinkID) && item.hyperlinkID > 0) {
          linkElement.href = `/hyperlinks/${encodeURIComponent(String(item.hyperlinkID))}`;
          linkElement.hidden = false;
          linkElement.classList.remove("hidden");
        } else {
          linkElement.hidden = true;
          linkElement.classList.add("hidden");
          linkElement.removeAttribute("href");
        }
      }

      nodes.push(row);
    }

    resultsList.replaceChildren(...nodes);
    resultsWrapper.hidden = false;
    resultsWrapper.classList.remove("hidden");
  }

  function setResultItems(items) {
    resultItems = Array.isArray(items) ? items : [];
    renderResults();
  }

  function createResultItems(files) {
    return files.map((file) => ({
      id: nextResultItemID++,
      file,
      state: "selected",
      message: "Ready to upload.",
      hyperlinkID: null,
    }));
  }

  function uploadProgressCounts(items) {
    const counts = {
      selected: 0,
      uploading: 0,
      success: 0,
      error: 0,
    };

    for (const item of items) {
      if (item?.state === "uploading") {
        counts.uploading += 1;
      } else if (item?.state === "success") {
        counts.success += 1;
      } else if (item?.state === "error") {
        counts.error += 1;
      } else {
        counts.selected += 1;
      }
    }

    return counts;
  }

  function setBusyUploadStatus(items) {
    const counts = uploadProgressCounts(items);
    const completed = counts.success + counts.error;
    const parts = [`${completed}/${items.length} finished`];
    if (counts.uploading > 0) {
      parts.push(`${counts.uploading} active`);
    }
    if (counts.error > 0) {
      parts.push(`${counts.error} failed`);
    }
    setStatus(`Uploading PDFs (${parts.join(", ")})…`, "busy");
  }

  function setUploadState(isUploading) {
    uploadInFlight = isUploading;
    root.dataset.uploading = isUploading ? "true" : "false";
    chooseButton.disabled = isUploading;
    fileInput.disabled = isUploading;
    titleInput.disabled = isUploading;
    submitButton.disabled = isUploading || selectedFiles.length === 0;
  }

  function setDragActive(isActive) {
    const value = isActive ? "true" : "false";
    root.dataset.dragActive = value;
    dropzone.dataset.dragActive = value;
    overlay.dataset.visible = value;
    overlay.hidden = !isActive;
    overlay.setAttribute("aria-hidden", isActive ? "false" : "true");
  }

  function resetWindowDragState() {
    windowDragDepth = 0;
    setDragActive(false);
  }

  function dataTransferHasFiles(dataTransfer) {
    if (!dataTransfer) {
      return false;
    }
    if (dataTransfer.files instanceof FileList && dataTransfer.files.length > 0) {
      return true;
    }
    return Array.from(dataTransfer.types || []).includes("Files");
  }

  function isPDFFile(file) {
    if (!(file instanceof File)) {
      return false;
    }
    const type = typeof file.type === "string" ? file.type.toLowerCase() : "";
    const name = typeof file.name === "string" ? file.name.toLowerCase() : "";
    return type === "application/pdf" || name.endsWith(".pdf");
  }

  async function readUploadError(response, fallbackMessage) {
    try {
      const text = await response.text();
      if (!text) {
        return fallbackMessage;
      }

      try {
        const payload = JSON.parse(text);
        if (payload && typeof payload.error === "string" && payload.error.trim().length > 0) {
          return payload.error.trim();
        }
        if (payload && typeof payload.message === "string" && payload.message.trim().length > 0) {
          return payload.message.trim();
        }
      } catch (_) {}

      const trimmed = text.trim();
      return trimmed || fallbackMessage;
    } catch (_) {
      return fallbackMessage;
    }
  }

  async function uploadSinglePDF(file, title) {
    if (!isPDFFile(file)) {
      throw new Error("Only PDF files can be uploaded.");
    }

    const filename = file.name && file.name.trim().length > 0 ? file.name.trim() : "document.pdf";
    const formData = new FormData();
    formData.append("upload_type", "pdf");
    if (title) {
      formData.append("title", title);
    }
    formData.append("filename", filename);
    formData.append("file", file, filename);

    let response;
    try {
      response = await fetch(form.action || "/uploads", {
        method: "POST",
        body: formData,
        credentials: "same-origin",
      });
    } catch (error) {
      const message = error instanceof Error && error.message
        ? error.message
        : "network error";
      throw new Error(`Upload failed for ${filename}: ${message}`);
    }

    if (!response.ok) {
      const message = await readUploadError(response, `Upload failed for ${filename}.`);
      throw new Error(message);
    }

    let payload;
    try {
      payload = await response.json();
    } catch (_) {
      throw new Error(`Upload succeeded for ${filename}, but the server response was invalid.`);
    }

    const hyperlinkID = Number(payload?.id);
    if (!Number.isFinite(hyperlinkID) || hyperlinkID < 1) {
      throw new Error(`Upload succeeded for ${filename}, but the hyperlink could not be opened.`);
    }

    return hyperlinkID;
  }

  async function uploadResultItems(items) {
    if (uploadInFlight) {
      setStatus("A PDF upload is already in progress.", "busy");
      return;
    }

    cancelPendingNavigation();

    const pdfItems = Array.isArray(items)
      ? items.filter((item) => item?.file instanceof File && isPDFFile(item.file))
      : [];

    if (pdfItems.length === 0) {
      setSelectedFiles([]);
      setResultItems([]);
      setStatus("Only PDF files can be uploaded.", "error");
      return;
    }

    setSelectedFiles(pdfItems.map((item) => item.file));
    setResultItems(pdfItems);
    setUploadState(true);
    setBusyUploadStatus(pdfItems);

    const createdIDs = [];
    const failedItems = [];
    const sharedTitle = pdfItems.length === 1 ? titleInput.value.trim() : "";
    let nextIndex = 0;

    async function runUploadWorker() {
      while (nextIndex < pdfItems.length) {
        const item = pdfItems[nextIndex];
        nextIndex += 1;

        const filename = item.file?.name?.trim() || "document.pdf";
        item.state = "uploading";
        item.hyperlinkID = null;
        item.message = "Uploading…";
        renderResults();
        setBusyUploadStatus(pdfItems);

        try {
          const hyperlinkID = await uploadSinglePDF(item.file, sharedTitle);
          item.state = "success";
          item.hyperlinkID = hyperlinkID;
          item.message = "Uploaded successfully.";
          createdIDs.push(hyperlinkID);
        } catch (error) {
          item.state = "error";
          item.hyperlinkID = null;
          item.message = error instanceof Error && error.message
            ? error.message
            : `Upload failed for ${filename}.`;
          failedItems.push(item);
        }

        renderResults();
        if (uploadProgressCounts(pdfItems).selected + uploadProgressCounts(pdfItems).uploading > 0) {
          setBusyUploadStatus(pdfItems);
        }
      }
    }

    const parallelism = Math.min(MAX_PARALLEL_PDF_UPLOADS, pdfItems.length);
    await Promise.all(
      Array.from({ length: parallelism }, () => runUploadWorker())
    );

    setUploadState(false);

    if (failedItems.length === 0) {
      setSelectedFiles([]);

      if (createdIDs.length === 1) {
        setStatus("Upload complete. Opening hyperlink…", "success");
        scheduleNavigation(() => {
          window.location.assign(`/hyperlinks/${encodeURIComponent(String(createdIDs[0]))}`);
        }, 350);
        return;
      }

      setStatus(`Uploaded ${createdIDs.length} PDFs with up to ${parallelism} uploads in parallel. Refreshing list…`, "success");
      scheduleNavigation(() => {
        if (window.location.pathname === "/hyperlinks" || window.location.pathname === "/hyperlinks/") {
          window.location.reload();
        } else {
          window.location.assign("/hyperlinks");
        }
      }, 900);
      return;
    }

    setSelectedFiles(failedItems.map((item) => item.file));

    const messageParts = [];
    if (createdIDs.length > 0) {
      messageParts.push(`Uploaded ${createdIDs.length} PDF${createdIDs.length === 1 ? "" : "s"}.`);
    }
    messageParts.push(`Failed to upload ${failedItems.length} PDF${failedItems.length === 1 ? "" : "s"}.`);
    const firstFailureMessage = failedItems[0]?.message?.trim();
    if (firstFailureMessage) {
      messageParts.push(firstFailureMessage);
    }
    if (createdIDs.length > 0) {
      messageParts.push("Successful uploads include links below.");
    }
    setStatus(messageParts.join(" "), "error");
  }

  function handleCandidateFiles(files, source) {
    if (uploadInFlight) {
      setStatus("A PDF upload is already in progress.", "busy");
      return;
    }

    cancelPendingNavigation();

    const pdfFiles = Array.isArray(files)
      ? files.filter((file) => file instanceof File && isPDFFile(file))
      : [];

    if (pdfFiles.length === 0) {
      setSelectedFiles([]);
      setResultItems([]);
      setStatus(source === "picker" ? defaultStatusMessage : "Only PDF files can be uploaded.", source === "picker" ? "idle" : "error");
      return;
    }

    const items = createResultItems(pdfFiles);
    setSelectedFiles(pdfFiles);
    setResultItems(items);

    if (source === "drop") {
      void uploadResultItems(items);
      return;
    }

    if (pdfFiles.length === 1) {
      setStatus(`Ready to upload ${pdfFiles[0].name}.`, "idle");
      return;
    }

    const titleWarning = titleInput.value.trim()
      ? " The title field will be ignored for multiple files."
      : "";
    setStatus(`Ready to upload ${pdfFiles.length} PDFs.${titleWarning}`, "idle");
  }

  chooseButton.addEventListener("click", () => {
    if (uploadInFlight) {
      return;
    }
    fileInput.click();
  });

  dropzone.addEventListener("click", (event) => {
    if (uploadInFlight) {
      return;
    }
    if (event.target instanceof Element && event.target.closest("button, input, a")) {
      return;
    }
    fileInput.click();
  });

  dropzone.addEventListener("keydown", (event) => {
    if (uploadInFlight) {
      return;
    }
    if (event.key !== "Enter" && event.key !== " ") {
      return;
    }
    event.preventDefault();
    fileInput.click();
  });

  fileInput.addEventListener("change", () => {
    const files = Array.from(fileInput.files || []);
    fileInput.value = "";
    handleCandidateFiles(files, "picker");
  });

  form.addEventListener("submit", (event) => {
    event.preventDefault();
    if (selectedFiles.length === 0) {
      setStatus("Choose at least one PDF before uploading.", "error");
      return;
    }
    void uploadResultItems(createResultItems(selectedFiles));
  });

  document.addEventListener("dragenter", (event) => {
    if (!(event instanceof DragEvent) || !dataTransferHasFiles(event.dataTransfer)) {
      return;
    }
    event.preventDefault();
    windowDragDepth += 1;
    if (!uploadInFlight) {
      setDragActive(true);
      setStatus("Drop one or more PDFs anywhere in this window to upload them.", "idle");
    }
  });

  document.addEventListener("dragover", (event) => {
    if (!(event instanceof DragEvent) || !dataTransferHasFiles(event.dataTransfer)) {
      return;
    }
    event.preventDefault();
    if (event.dataTransfer) {
      event.dataTransfer.dropEffect = uploadInFlight ? "none" : "copy";
    }
    if (!uploadInFlight) {
      setDragActive(true);
    }
  });

  document.addEventListener("dragleave", (event) => {
    if (!(event instanceof DragEvent)) {
      return;
    }
    if (windowDragDepth === 0 && !dataTransferHasFiles(event.dataTransfer)) {
      return;
    }
    windowDragDepth = Math.max(0, windowDragDepth - 1);
    if (windowDragDepth === 0) {
      setDragActive(false);
      if (!uploadInFlight && selectedFiles.length === 0 && resultItems.length === 0) {
        setStatus(defaultStatusMessage, "idle");
      }
    }
  });

  document.addEventListener("drop", (event) => {
    if (!(event instanceof DragEvent) || !dataTransferHasFiles(event.dataTransfer)) {
      return;
    }
    event.preventDefault();
    const files = Array.from(event.dataTransfer?.files || []);
    resetWindowDragState();
    handleCandidateFiles(files, "drop");
  });

  window.addEventListener("blur", resetWindowDragState);
  window.addEventListener("dragend", resetWindowDragState);

  setSelectedFiles([]);
  setResultItems([]);
  setUploadState(false);
  setStatus(defaultStatusMessage, "idle");
}

initializeUrlIntent();
initializePDFUpload();
initializeLlmModelDiscovery();

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

const ADMIN_STATUS_EVENT = "admin:status";
let adminStatusRequest = null;

function dispatchAdminStatus(payload) {
  window.dispatchEvent(
    new CustomEvent(ADMIN_STATUS_EVENT, {
      detail: payload,
    }),
  );
}

async function refreshAdminStatus() {
  if (adminStatusRequest) {
    return adminStatusRequest;
  }

  adminStatusRequest = (async () => {
    try {
      const response = await fetch("/admin/status", {
        method: "GET",
        credentials: "same-origin",
        cache: "no-store",
      });
      if (!response.ok) {
        return;
      }

      const data = await response.json();
      dispatchAdminStatus(data);
    } catch (_) {}
  })();

  try {
    await adminStatusRequest;
  } finally {
    adminStatusRequest = null;
  }
}

function formatBackupStageLabel(stage) {
  if (stage === "loading_records") {
    return "Loading records";
  }
  if (stage === "packing_artifacts") {
    return "Packing artifacts";
  }
  if (stage === "finalizing") {
    return "Finalizing archive";
  }
  return "Working";
}

function formatImportStageLabel(stage) {
  if (stage === "validating") {
    return "Validating backup";
  }
  if (stage === "restoring_hyperlinks") {
    return "Restoring hyperlinks";
  }
  if (stage === "restoring_relations") {
    return "Restoring relations";
  }
  if (stage === "restoring_artifacts") {
    return "Restoring artifacts";
  }
  if (stage === "finalizing") {
    return "Finalizing import";
  }
  return "Working";
}

function initializeAdminBackupControls() {
  const container = document.querySelector("[data-admin-backup]");
  if (!(container instanceof HTMLElement)) {
    return false;
  }

  const createButton = container.querySelector("[data-admin-backup-create]");
  const cancelButton = container.querySelector("[data-admin-backup-cancel]");
  const downloadLink = container.querySelector("[data-admin-backup-download]");
  const statusText = container.querySelector("[data-admin-backup-status]");
  const progressText = container.querySelector("[data-admin-backup-progress]");

  if (
    !(createButton instanceof HTMLButtonElement) ||
    !(cancelButton instanceof HTMLButtonElement) ||
    !(downloadLink instanceof HTMLAnchorElement) ||
    !(statusText instanceof HTMLElement) ||
    !(progressText instanceof HTMLElement)
  ) {
    return false;
  }

  let actionInFlight = false;
  let latestBackup = null;

  const applyBackupStatus = (backup) => {
    latestBackup = backup;
    const state = typeof backup?.state === "string" ? backup.state : "idle";
    const isRunning = state === "running";
    const hasDownload = backup?.download_ready === true;

    createButton.disabled = actionInFlight || isRunning;
    cancelButton.disabled = actionInFlight || !isRunning;
    cancelButton.classList.toggle("hidden", !isRunning);
    downloadLink.classList.toggle("hidden", !hasDownload);
    downloadLink.setAttribute("href", "/admin/export/download");

    if (state === "running") {
      const stage = typeof backup?.stage === "string" ? backup.stage : "";
      const artifactsDone = Number(backup?.artifacts_done);
      const artifactsTotal = Number(backup?.artifacts_total);
      const stageLabel = formatBackupStageLabel(stage);

      statusText.textContent = "Creating backup ZIP...";
      if (
        Number.isFinite(artifactsDone) &&
        Number.isFinite(artifactsTotal) &&
        artifactsTotal > 0
      ) {
        progressText.textContent = `${stageLabel}: ${artifactsDone}/${artifactsTotal} artifacts`;
      } else {
        progressText.textContent = stageLabel;
      }
      progressText.classList.remove("hidden");
      return;
    }

    progressText.classList.add("hidden");
    progressText.textContent = "";

    if (state === "ready") {
      const hyperlinks = Number(backup?.hyperlinks);
      const relations = Number(backup?.relations);
      const artifacts = Number(backup?.artifacts);
      if (
        Number.isFinite(hyperlinks) &&
        Number.isFinite(relations) &&
        Number.isFinite(artifacts)
      ) {
        statusText.textContent = `Backup ready (${hyperlinks} links, ${relations} relations, ${artifacts} artifacts).`;
      } else {
        statusText.textContent = "Backup ready.";
      }
      return;
    }

    if (state === "failed") {
      const error =
        typeof backup?.error === "string" && backup.error.trim().length > 0
          ? backup.error.trim()
          : "unknown error";
      statusText.textContent = hasDownload
        ? `Backup failed: ${error}. Last completed backup is still available for download.`
        : `Backup failed: ${error}`;
      return;
    }

    if (state === "cancelled") {
      statusText.textContent = hasDownload
        ? "Backup cancelled. Last completed backup is still available for download."
        : "Backup cancelled.";
      return;
    }

    statusText.textContent = hasDownload
      ? "Backup is idle. A backup ZIP is available for download."
      : "Backup is idle.";
  };

  async function postCreateBackup() {
    if (actionInFlight) {
      return;
    }

    actionInFlight = true;
    applyBackupStatus(latestBackup);
    try {
      await fetch("/admin/export/start", {
        method: "POST",
        credentials: "same-origin",
      });
    } catch (_) {
    } finally {
      actionInFlight = false;
      applyBackupStatus(latestBackup);
      await refreshAdminStatus();
    }
  }

  createButton.addEventListener("click", () => {
    void postCreateBackup();
  });

  window.addEventListener(ADMIN_STATUS_EVENT, (event) => {
    if (!(event instanceof CustomEvent)) {
      return;
    }
    const backup = event.detail?.backup;
    applyBackupStatus(backup);
  });

  applyBackupStatus(null);
  return true;
}

function initializeAdminImportControls() {
  const container = document.querySelector("[data-admin-import]");
  if (!(container instanceof HTMLElement)) {
    return false;
  }

  const form = container.querySelector("[data-admin-import-form]");
  const submitButton = container.querySelector("[data-admin-import-submit]");
  const cancelButton = container.querySelector("[data-admin-import-cancel]");
  const statusText = container.querySelector("[data-admin-import-status]");
  const progressText = container.querySelector("[data-admin-import-progress]");
  const fileInput = container.querySelector("#admin-import-archive");

  if (
    !(form instanceof HTMLFormElement) ||
    !(submitButton instanceof HTMLButtonElement) ||
    !(cancelButton instanceof HTMLButtonElement) ||
    !(statusText instanceof HTMLElement) ||
    !(progressText instanceof HTMLElement)
  ) {
    return false;
  }

  let uploadInFlight = false;
  let latestImport = null;

  const applyImportStatus = (status) => {
    latestImport = status;
    const state = typeof status?.state === "string" ? status.state : "idle";
    const isRunning = state === "running";

    submitButton.disabled = uploadInFlight || isRunning;
    cancelButton.disabled = !isRunning;
    cancelButton.classList.toggle("hidden", !isRunning);
    if (fileInput instanceof HTMLInputElement) {
      // Keep the file input enabled during submit so the browser includes
      // the selected file part in multipart form serialization.
      fileInput.disabled = isRunning;
    }

    if (state === "running") {
      const stage = typeof status?.stage === "string" ? status.stage : "";
      const stageLabel = formatImportStageLabel(stage);
      statusText.textContent = "Importing backup ZIP...";

      if (stage === "restoring_hyperlinks") {
        const done = Number(status?.hyperlinks_done);
        const total = Number(status?.hyperlinks_total);
        if (Number.isFinite(done) && Number.isFinite(total) && total >= 0) {
          progressText.textContent = `${stageLabel}: ${done}/${total}`;
        } else {
          progressText.textContent = stageLabel;
        }
      } else if (stage === "restoring_relations") {
        const done = Number(status?.relations_done);
        const total = Number(status?.relations_total);
        if (Number.isFinite(done) && Number.isFinite(total) && total >= 0) {
          progressText.textContent = `${stageLabel}: ${done}/${total}`;
        } else {
          progressText.textContent = stageLabel;
        }
      } else if (stage === "restoring_artifacts") {
        const done = Number(status?.artifacts_done);
        const total = Number(status?.artifacts_total);
        if (Number.isFinite(done) && Number.isFinite(total) && total >= 0) {
          progressText.textContent = `${stageLabel}: ${done}/${total}`;
        } else {
          progressText.textContent = stageLabel;
        }
      } else {
        progressText.textContent = stageLabel;
      }

      progressText.classList.remove("hidden");
      return;
    }

    progressText.classList.add("hidden");
    progressText.textContent = "";

    if (state === "ready") {
      const hyperlinks = Number(status?.hyperlinks);
      const relations = Number(status?.relations);
      const artifacts = Number(status?.artifacts);
      if (
        Number.isFinite(hyperlinks) &&
        Number.isFinite(relations) &&
        Number.isFinite(artifacts)
      ) {
        statusText.textContent = `Import complete (${hyperlinks} links, ${relations} relations, ${artifacts} artifacts).`;
      } else {
        statusText.textContent = "Import complete.";
      }
      return;
    }

    if (state === "failed") {
      const error =
        typeof status?.error === "string" && status.error.trim().length > 0
          ? status.error.trim()
          : "unknown error";
      statusText.textContent = `Import failed: ${error}`;
      return;
    }

    if (state === "cancelled") {
      statusText.textContent = "Import canceled.";
      return;
    }

    statusText.textContent = "Import is idle.";
  };

  form.addEventListener("submit", () => {
    uploadInFlight = true;
    applyImportStatus(latestImport);
    statusText.textContent = "Uploading backup ZIP...";
  });

  window.addEventListener(ADMIN_STATUS_EVENT, (event) => {
    if (!(event instanceof CustomEvent)) {
      return;
    }
    const status = event.detail?.import;
    applyImportStatus(status);
  });

  applyImportStatus(null);
  return true;
}

async function readAdminError(response, fallbackMessage) {
  try {
    const payload = await response.json();
    if (payload && typeof payload.error === "string" && payload.error.trim().length > 0) {
      return payload.error.trim();
    }
    if (payload && typeof payload.message === "string" && payload.message.trim().length > 0) {
      return payload.message.trim();
    }
  } catch (_) {}
  return fallbackMessage;
}

function initializeAdminStatusPolling() {
  const hasQueueBadge = document.querySelector("[data-queue-pending-badge]");
  const hasBackupControls = initializeAdminBackupControls();
  const hasImportControls = initializeAdminImportControls();

  if (
    !hasQueueBadge &&
    !hasBackupControls &&
    !hasImportControls
  ) {
    return;
  }

  window.addEventListener(ADMIN_STATUS_EVENT, (event) => {
    if (!(event instanceof CustomEvent)) {
      return;
    }
    const pending = Number(event.detail?.queue?.pending);
    updateQueuePendingBadge(pending);
  });

  void refreshAdminStatus();
  setInterval(() => {
    if (document.visibilityState !== "visible") {
      return;
    }
    void refreshAdminStatus();
  }, 5000);

  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible") {
      void refreshAdminStatus();
    }
  });
}

initializeAdminStatusPolling();
