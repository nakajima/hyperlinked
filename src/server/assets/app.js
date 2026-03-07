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

function initializeTaggingModelDiscovery() {
  const form = document.querySelector("[data-tagging-settings-form]");
  if (!(form instanceof HTMLFormElement)) {
    return;
  }

  const baseUrlInput = form.querySelector("[data-tagging-base-url]");
  const modelSelect = form.querySelector("[data-tagging-model-select]");
  const apiKeyInput = form.querySelector("[data-tagging-api-key]");
  const authHeaderNameInput = form.querySelector("[data-tagging-auth-header-name]");
  const authHeaderPrefixInput = form.querySelector("[data-tagging-auth-header-prefix]");
  const backendKindInput = form.querySelector("[data-tagging-backend-kind]");
  const modelStatus = form.querySelector("[data-tagging-model-status]");

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
    if (!(backendKindInput instanceof HTMLInputElement)) {
      return;
    }
    backendKindInput.value = trimmedOrEmpty(kind) || "unknown";
  };

  const selectedModel = () => {
    const value = trimmedOrEmpty(modelSelect.value);
    if (value) {
      return value;
    }
    const selected = modelSelect.selectedOptions.item(0);
    if (!selected) {
      return "";
    }
    return trimmedOrEmpty(selected.value || selected.textContent || "");
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
      const response = await fetch("/admin/tagging-models", {
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
      updateBackendKind(backendKind);
      applyModelOptions(models, previousSelection);

      const backendText = backendKindLabel(backendKind);
      if (models.length > 0) {
        setStatus(
          `Detected ${backendText} backend. Loaded ${models.length} model option${models.length === 1 ? "" : "s"}.`,
        );
      } else {
        setStatus(
          `Detected ${backendText} backend. Endpoint returned no models. Keeping the current model.`,
        );
      }
    } catch (error) {
      if (error instanceof DOMException && error.name === "AbortError") {
        return;
      }
      if (lookupRequestId !== latestLookupRequestId) {
        return;
      }
      setStatus("Could not load models from the endpoint.");
    } finally {
      if (activeController === controller) {
        activeController = null;
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
    }, 350);
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

initializeUrlIntent();
initializeTaggingModelDiscovery();

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

function initializeAdminTagReclassifyControls() {
  const container = document.querySelector("[data-admin-tag-reclassify]");
  if (!(container instanceof HTMLElement)) {
    return false;
  }

  const startButton = container.querySelector("[data-admin-tag-reclassify-start]");
  const cancelButton = container.querySelector("[data-admin-tag-reclassify-cancel]");
  const statusText = container.querySelector("[data-admin-tag-reclassify-status]");
  const progressText = container.querySelector("[data-admin-tag-reclassify-progress]");

  if (
    !(startButton instanceof HTMLButtonElement) ||
    !(cancelButton instanceof HTMLButtonElement) ||
    !(statusText instanceof HTMLElement) ||
    !(progressText instanceof HTMLElement)
  ) {
    return false;
  }

  let actionInFlight = false;
  let latestStatus = null;
  let previousState = "idle";
  let reloadedAfterReady = false;

  const applyStatus = (status) => {
    latestStatus = status;
    const state = typeof status?.state === "string" ? status.state : "idle";
    const isRunning = state === "running";
    startButton.disabled = actionInFlight || isRunning;
    cancelButton.disabled = actionInFlight || !isRunning;
    cancelButton.classList.toggle("hidden", !isRunning);

    if (isRunning) {
      const processed = Number(status?.processed);
      const total = Number(status?.total);
      statusText.textContent = "Reclassifying tags...";
      if (Number.isFinite(processed) && Number.isFinite(total) && total >= 0) {
        progressText.textContent = `${processed}/${total} links`;
      } else {
        progressText.textContent = "Working";
      }
      progressText.classList.remove("hidden");
      return;
    }

    progressText.classList.add("hidden");
    progressText.textContent = "";

    if (state === "ready") {
      const processed = Number(status?.processed);
      const total = Number(status?.total);
      if (Number.isFinite(processed) && Number.isFinite(total) && total >= 0) {
        statusText.textContent = `Reclassify complete (${processed}/${total} links).`;
      } else {
        statusText.textContent = "Reclassify complete.";
      }
      if (previousState === "running" && !reloadedAfterReady) {
        reloadedAfterReady = true;
        window.setTimeout(() => {
          window.location.reload();
        }, 350);
      }
      previousState = state;
      return;
    }

    if (state === "failed") {
      const error =
        typeof status?.error === "string" && status.error.trim().length > 0
          ? status.error.trim()
          : "unknown error";
      statusText.textContent = `Reclassify failed: ${error}`;
      previousState = state;
      return;
    }

    if (state === "cancelled") {
      statusText.textContent = "Reclassify canceled.";
      previousState = state;
      return;
    }

    statusText.textContent = "Reclassify is idle.";
    previousState = state;
  };

  async function postReclassifyStart() {
    if (actionInFlight) {
      return;
    }
    actionInFlight = true;
    applyStatus(latestStatus);
    try {
      await fetch("/admin/tags/reclassify/start", {
        method: "POST",
        credentials: "same-origin",
      });
    } catch (_) {
    } finally {
      actionInFlight = false;
      applyStatus(latestStatus);
      await refreshAdminStatus();
    }
  }

  async function postReclassifyCancel() {
    if (actionInFlight) {
      return;
    }
    actionInFlight = true;
    applyStatus(latestStatus);
    try {
      await fetch("/admin/tags/reclassify/cancel", {
        method: "POST",
        credentials: "same-origin",
      });
    } catch (_) {
    } finally {
      actionInFlight = false;
      applyStatus(latestStatus);
      await refreshAdminStatus();
    }
  }

  startButton.addEventListener("click", () => {
    void postReclassifyStart();
  });
  cancelButton.addEventListener("click", () => {
    void postReclassifyCancel();
  });

  window.addEventListener(ADMIN_STATUS_EVENT, (event) => {
    if (!(event instanceof CustomEvent)) {
      return;
    }
    applyStatus(event.detail?.tag_reclassify);
  });

  applyStatus(null);
  return true;
}

function initializeAdminStatusPolling() {
  const hasQueueBadge = document.querySelector("[data-queue-pending-badge]");
  const hasBackupControls = initializeAdminBackupControls();
  const hasImportControls = initializeAdminImportControls();
  const hasTagReclassifyControls = initializeAdminTagReclassifyControls();

  if (
    !hasQueueBadge &&
    !hasBackupControls &&
    !hasImportControls &&
    !hasTagReclassifyControls
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
