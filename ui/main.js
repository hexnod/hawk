// Copyright 2026 Mark West
// SPDX-License-Identifier: Apache-2.0

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { getCurrentWindow } = window.__TAURI__.window;
const dialog = window.__TAURI__.dialog;

const APP_NAME = "Hawk";
const REPO_URL = "https://github.com/hexnod/hawk";

const editor = document.getElementById("editor");
const previewFrame = document.getElementById("preview-frame");
const previewToggle = document.getElementById("preview-toggle");
const statusDocType = document.getElementById("status-doctype");
const statusMessageText = document.getElementById("status-message-text");
const statusMoreLink = document.getElementById("status-more-link");
const statusSaveState = document.getElementById("status-savestate");
const statusZoom = document.getElementById("status-zoom");
const statusResizeGrip = document.getElementById("status-resize-grip");
const appWindow = getCurrentWindow();

// Previewable extensions -> status bar document type.
const PREVIEW_DOC_TYPES = { md: "Markdown", html: "HyperText", htm: "HyperText" };

let currentPath = null;
let lastKnownDir = null;
let isDirty = false;
let wordWrap = true;
let spellcheck = true;
let tabMode = "None"; // "None" | "Tab" | "Spaces"
let theme = "Default"; // "Default" | "Light" | "Dark"
let currentZoom = 1.0;
let currentDocType = ""; // "" | "Markdown" | "HyperText"
let previewOn = false;
let lastPreviewErrorDetail = "";
let linkHoverSavedStatus = null; // status area to restore after a preview link hover
let previewToken = ""; // identifies messages from the current preview's bridge script

function basename(p) {
  if (!p) return "Untitled";
  const parts = p.split(/[\\/]/);
  return parts[parts.length - 1] || p;
}

function dirOf(p) {
  if (!p) return null;
  const idx = Math.max(p.lastIndexOf("/"), p.lastIndexOf("\\"));
  if (idx <= 0) return null;
  return p.substring(0, idx + 1);
}

async function updateTitle() {
  const name = basename(currentPath);
  const title = `${isDirty ? "*" : ""}${name} - ${APP_NAME}`;
  try {
    await appWindow.setTitle(title);
  } catch (e) {
    document.title = title;
  }
}

function setDirty(value) {
  if (isDirty !== value) {
    isDirty = value;
    updateTitle();
  }
  statusSaveState.textContent = isDirty ? "Unsaved" : "Saved";
}

function applyWordWrap() {
  editor.classList.toggle("wrap-on", wordWrap);
  editor.classList.toggle("wrap-off", !wordWrap);
}

// ---- Editor font ----
// The "font" setting has no UI; it's edited in settings.json. A font that
// isn't installed falls back to the stylesheet's default (Consolas) and
// reports an error in the status bar.
const DEFAULT_FONT = "Consolas";

// Browsers don't say whether a local font exists, so compare text widths:
// if "<font>, <generic>" always measures like the generic alone, the
// font wasn't used.
function isFontInstalled(cssName) {
  const ctx = document.createElement("canvas").getContext("2d");
  const sample = "mmmmmmmmmmlli1WQ@#";
  const width = (family) => {
    ctx.font = `72px ${family}`;
    return ctx.measureText(sample).width;
  };
  return ["monospace", "sans-serif", "serif"].some((g) => width(`${cssName}, ${g}`) !== width(g));
}

// Returns an error message, or "" if the font was applied (or is the default).
function applyFont(font) {
  editor.style.fontFamily = "";
  const name = typeof font === "string" ? font.trim() : "";
  if (!name || name === DEFAULT_FONT) return "";
  const cssName = `"${name.replace(/["\\]/g, "\\$&")}"`;
  if (!isFontInstalled(cssName)) {
    return `Font "${name}" not found; using ${DEFAULT_FONT}.`;
  }
  editor.style.fontFamily = `${cssName}, Consolas, "Courier New", monospace`;
  return "";
}

function applySpellcheck() {
  editor.setAttribute("spellcheck", spellcheck ? "true" : "false");
  editor.spellcheck = spellcheck;
}

function applyTheme() {
  document.body.classList.remove("theme-light", "theme-dark");
  if (theme === "Light") {
    document.body.classList.add("theme-light");
  } else if (theme === "Dark") {
    document.body.classList.add("theme-dark");
  }
  // "Default" adds no class and follows the OS via prefers-color-scheme.
  if (previewOn) {
    renderPreview(); // pick up the new palette
  }
}

editor.addEventListener("keydown", (event) => {
  if (event.key !== "Tab" || event.shiftKey) return;
  if (tabMode === "None") return;
  event.preventDefault();
  const insertion = tabMode === "Tab" ? "\t" : "    ";
  document.execCommand("insertText", false, insertion);
});

// Keyboard shortcuts. Native menu accelerators don't fire reliably while
// WebView2 has focus, so they're handled here. shortcutAction must stay
// self-contained: it's also injected into the preview (see
// PREVIEW_BRIDGE_SCRIPT), which has its own keyboard focus.
function shortcutAction(key, code) {
  key = key.toLowerCase();
  if (key === "n") return "new";
  if (key === "o") return "open";
  if (key === "s") return "save";
  if (key === "+" || key === "=" || code === "NumpadAdd") return "zoom_in";
  if (key === "-" || code === "NumpadSubtract") return "zoom_out";
  if (key === "0" || code === "Numpad0") return "zoom_reset";
  return null;
}

function runShortcut(action) {
  if (action === "new") doNew();
  else if (action === "open") doOpen();
  else if (action === "save") doSave();
  else invoke(action).then(updateZoomDisplay).catch(() => {});
}

window.addEventListener("keydown", (event) => {
  if (!(event.ctrlKey || event.metaKey) || event.altKey) return;
  const action = shortcutAction(event.key, event.code);
  if (!action) return;
  event.preventDefault();
  runShortcut(action);
});

function updateZoomDisplay(factor) {
  if (typeof factor === "number" && !Number.isNaN(factor)) {
    currentZoom = factor;
  }
  statusZoom.textContent = `${Math.round(currentZoom * 100)}%`;
}

// ---------------- Markdown / HyperText preview ----------------

function getExtension(path) {
  if (!path) return "";
  const base = basename(path);
  const idx = base.lastIndexOf(".");
  if (idx <= 0) return ""; // none, or a dotfile
  return base.substring(idx + 1).toLowerCase();
}

function computeDocType(path) {
  const ext = getExtension(path);
  return PREVIEW_DOC_TYPES[ext] || "";
}

function escapeHtml(str) {
  return String(str)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}

// Preview palette. CSS variables don't cross into the iframe, so concrete
// colors are inlined into the generated document.
function getActiveThemeColors() {
  let dark;
  if (theme === "Dark") {
    dark = true;
  } else if (theme === "Light") {
    dark = false;
  } else {
    dark = !!(window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)").matches);
  }
  return dark
    ? { bg: "#1e1e1e", fg: "#d4d4d4", link: "#6cb6ff", codeBg: "#2a2a2a", border: "#4a4a4a", mutedFg: "#9a9a9a" }
    : { bg: "#ffffff", fg: "#1f1f1f", link: "#0645ad", codeBg: "#f2f2f2", border: "#d0d0d0", mutedFg: "#5a5a5a" };
}

// Wraps Markdown output or an HTML fragment in a minimal themed page.
function wrapThemedDocument(bodyHtml, isMarkdown) {
  const c = getActiveThemeColors();
  const markdownCss = isMarkdown
    ? `
      h1, h2, h3, h4, h5, h6 { line-height: 1.25; margin-top: 1.2em; margin-bottom: 0.5em; }
      code, pre { font-family: Consolas, "Courier New", monospace; }
      code { background: ${c.codeBg}; padding: 0.15em 0.35em; border-radius: 3px; }
      pre { background: ${c.codeBg}; padding: 10px 12px; border-radius: 4px; overflow: auto; }
      pre code { background: none; padding: 0; }
      blockquote { margin: 0.5em 0; padding: 0.2em 0 0.2em 12px; border-left: 4px solid ${c.border}; color: ${c.mutedFg}; }
      table { border-collapse: collapse; }
      th, td { border: 1px solid ${c.border}; padding: 4px 8px; }
      hr { border: none; border-top: 1px solid ${c.border}; }
      img { max-width: 100%; }
    `
    : "";
  return `<!DOCTYPE html><html><head><meta charset="utf-8"><style>
    html, body { margin: 0; }
    body {
      box-sizing: border-box;
      padding: 16px;
      font-family: "Segoe UI", Tahoma, Arial, sans-serif;
      background: ${c.bg};
      color: ${c.fg};
      line-height: 1.5;
    }
    a { color: ${c.link}; }
    ${markdownCss}
  </style></head><body>${bodyHtml}</body></html>`;
}

// A full HTML document (has <html>) is shown as authored; a fragment gets
// the themed shell. Resources with relative paths aren't loaded from disk.
function buildHtmlPreviewDocument(raw) {
  if (/<html[\s>]/i.test(raw)) {
    return raw;
  }
  return wrapThemedDocument(raw, false);
}

function setStatusError(shortMessage, detail) {
  linkHoverSavedStatus = null;
  lastPreviewErrorDetail = detail || shortMessage || "";
  statusMessageText.innerHTML = `<span class="status-icon-error" aria-hidden="true">⚠</span>${escapeHtml(shortMessage)}`;
  statusMessageText.classList.add("is-error");
  statusMoreLink.classList.remove("hidden");
}

function setStatusInfo(message) {
  clearStatusError();
  statusMessageText.textContent = message;
}

function clearStatusError() {
  linkHoverSavedStatus = null;
  lastPreviewErrorDetail = "";
  statusMessageText.textContent = "";
  statusMessageText.classList.remove("is-error");
  statusMoreLink.classList.add("hidden");
}

// ---- Preview bridge ----
// The preview iframe is sandboxed (opaque origin), so Hawk can't reach its
// DOM. This script, injected first into every preview document, reports
// link hovers, handles link clicks and forwards shortcuts by postMessage.
//
// Clicks never navigate the iframe: its relative URLs resolve against
// Hawk's own page and Tauri answers unknown paths with index.html, which
// showed a broken copy of Hawk's UI. In-page "#anchors" scroll; other
// links go to openPreviewLink.
//
// The script removes itself and keeps the token in a closure, so the
// previewed page's own scripts can't forge messages.
const PREVIEW_BRIDGE_SCRIPT = `<script>(function (token, shortcutAction) {
  document.currentScript.remove();
  function send(msg) { msg.token = token; try { parent.postMessage(msg, "*"); } catch (e) {} }
  function linkOf(node) { return node && node.closest ? node.closest("a[href]") : null; }
  var lastHref;
  function hover(href) {
    if (href === lastHref) return;
    lastHref = href;
    send({ type: "hover", href: href });
  }
  document.addEventListener("mouseover", function (e) {
    var a = linkOf(e.target);
    hover(a ? a.getAttribute("href") : null);
  }, true);
  document.addEventListener("mouseout", function (e) {
    if (!e.relatedTarget) hover(null); // left the document
  }, true);
  document.addEventListener("click", function (e) {
    var a = linkOf(e.target);
    if (!a) return;
    var href = a.getAttribute("href").trim();
    if (/^javascript:/i.test(href)) return; // runs inside the sandbox
    e.preventDefault();
    if (!href) return;
    if (href.charAt(0) === "#") {
      var id = decodeURIComponent(href.slice(1));
      var el = id ? document.getElementById(id) || document.getElementsByName(id)[0] : null;
      if (el) el.scrollIntoView();
      else if (id === "" || id.toLowerCase() === "top") window.scrollTo(0, 0);
      return;
    }
    send({ type: "link", href: href });
  }, true);
  document.addEventListener("keydown", function (e) {
    if (!(e.ctrlKey || e.metaKey) || e.altKey) return;
    var action = shortcutAction(e.key, e.code);
    if (!action) return;
    e.preventDefault();
    send({ type: "shortcut", action: action });
  }, true);
})(__TOKEN__, ${shortcutAction.toString()});<\/script>`;

// Inserts the bridge right after <head> (else <html>, else at the start),
// never before <!DOCTYPE>, which would force quirks mode.
function injectPreviewBridge(doc) {
  previewToken = Array.from(crypto.getRandomValues(new Uint8Array(16)), (b) =>
    b.toString(16).padStart(2, "0")
  ).join("");
  const script = PREVIEW_BRIDGE_SCRIPT.replace("__TOKEN__", JSON.stringify(previewToken));
  const anchor = doc.match(/<head(\s[^>]*)?>/i) || doc.match(/<html(\s[^>]*)?>/i);
  if (!anchor) return script + doc;
  const at = anchor.index + anchor[0].length;
  return doc.slice(0, at) + script + doc.slice(at);
}

function showLinkHover(href) {
  if (href) {
    if (!linkHoverSavedStatus) {
      linkHoverSavedStatus = {
        html: statusMessageText.innerHTML,
        isError: statusMessageText.classList.contains("is-error"),
        moreHidden: statusMoreLink.classList.contains("hidden"),
      };
    }
    statusMessageText.textContent = href;
    statusMessageText.classList.remove("is-error");
    statusMoreLink.classList.add("hidden");
  } else if (linkHoverSavedStatus) {
    const saved = linkHoverSavedStatus;
    linkHoverSavedStatus = null;
    statusMessageText.innerHTML = saved.html;
    statusMessageText.classList.toggle("is-error", saved.isError);
    statusMoreLink.classList.toggle("hidden", saved.moreHidden);
  }
}

window.addEventListener("message", (event) => {
  if (event.source !== previewFrame.contentWindow) return;
  const data = event.data;
  if (!data || !previewToken || data.token !== previewToken) return;
  if (data.type === "hover") {
    showLinkHover(typeof data.href === "string" && data.href !== "" ? data.href : null);
  } else if (data.type === "link" && typeof data.href === "string") {
    openPreviewLink(data.href);
  } else if (data.type === "shortcut") {
    if (["new", "open", "save", "zoom_in", "zoom_out", "zoom_reset"].includes(data.action)) {
      runShortcut(data.action);
    }
  }
});

// Web and mail links open in the default browser/mail app; anything else
// is treated as a file path and opened in a new Hawk instance (in preview
// if it's previewable). Failures go to the status bar.
async function openPreviewLink(href) {
  const scheme = /^([a-z][a-z0-9+.-]+):/i.exec(href); // 2+ chars, so "C:\x" is a path
  const lower = scheme ? scheme[1].toLowerCase() : "";
  try {
    if (lower === "http" || lower === "https" || lower === "mailto") {
      await invoke("open_external_url", { url: href });
    } else if (scheme && lower !== "file") {
      setStatusInfo(`Unsupported link: ${href}`);
    } else {
      const preview = computeDocType(href.split(/[?#]/)[0]) !== "";
      await invoke("open_linked_file", { docPath: currentPath, href, preview });
    }
  } catch (e) {
    setStatusInfo(String(e));
  }
}

// In case the iframe misses the pointer leaving (e.g. a fast move).
previewFrame.addEventListener("mouseleave", () => showLinkHover(null));

statusMoreLink.addEventListener("click", (event) => {
  event.preventDefault();
  showErrorModal(lastPreviewErrorDetail || "No further details are available.");
});

function renderPreview() {
  clearStatusError();
  const raw = editor.value;
  try {
    let doc;
    if (currentDocType === "Markdown") {
      const body = marked.parse(raw);
      doc = wrapThemedDocument(body, true);
    } else if (currentDocType === "HyperText") {
      doc = buildHtmlPreviewDocument(raw);
    } else {
      doc = wrapThemedDocument("<p><em>Preview not available for this file type.</em></p>", false);
    }
    previewFrame.srcdoc = injectPreviewBridge(doc);
  } catch (err) {
    const message = err && err.message ? err.message : String(err);
    setStatusError("Failed to render document.", message);
    invoke("log_event", {
      message: `Preview render error (${currentDocType || "unknown"}): ${message}`,
    }).catch(() => {});
    // Show the raw text rather than a blank panel.
    previewFrame.srcdoc = wrapThemedDocument(
      `<pre style="white-space:pre-wrap;">${escapeHtml(raw)}</pre>`,
      false
    );
  }
}

function showEditMode() {
  previewFrame.classList.add("hidden");
  editor.classList.remove("hidden");
  editor.focus();
}

function showPreviewMode() {
  editor.classList.add("hidden");
  previewFrame.classList.remove("hidden");
}

function updatePreviewAvailabilityUI() {
  const supported = currentDocType !== "";
  previewToggle.disabled = !supported;
  previewToggle.setAttribute("aria-checked", String(previewOn));
  statusDocType.textContent = currentDocType;
}

function setPreviewMode(on) {
  if (on === previewOn) return;
  if (on && currentDocType === "") return;
  previewOn = on;
  previewToggle.setAttribute("aria-checked", String(previewOn));
  invoke("set_text_ops_enabled", { enabled: !previewOn }).catch(() => {});
  if (previewOn) {
    invoke("log_event", { message: `Preview enabled (${currentDocType})` }).catch(() => {});
    renderPreview();
    showPreviewMode();
  } else {
    invoke("log_event", { message: "Preview disabled" }).catch(() => {});
    clearStatusError();
    showEditMode();
  }
}

previewToggle.addEventListener("click", () => {
  if (previewToggle.disabled) return;
  setPreviewMode(!previewOn);
});

// On Open and Save As (not plain Save): preview starts off for the new file.
function resetPreviewForNewDocument() {
  currentDocType = computeDocType(currentPath);
  previewOn = false;
  updatePreviewAvailabilityUI();
  showEditMode();
  invoke("set_text_ops_enabled", { enabled: true }).catch(() => {});
}

// ---------------- Custom modal dialogs (Yes/No/Cancel, OK) ----------------
const overlay = document.getElementById("modal-overlay");
const modalTitle = document.getElementById("modal-title");
const modalMessage = document.getElementById("modal-message");
const modalButtons = document.getElementById("modal-buttons");

// `message` is text, or a DOM node for rich content (the About box's link).
function showModal(title, message, buttons) {
  return new Promise((resolve) => {
    modalTitle.textContent = title;
    if (message instanceof Node) {
      modalMessage.replaceChildren(message);
    } else {
      modalMessage.textContent = message;
    }
    modalButtons.innerHTML = "";
    buttons.forEach((label, i) => {
      const btn = document.createElement("button");
      btn.textContent = label;
      btn.addEventListener("click", () => {
        overlay.classList.add("hidden");
        resolve(label);
      });
      modalButtons.appendChild(btn);
      if (i === 0) {
        setTimeout(() => btn.focus(), 0);
      }
    });
    overlay.classList.remove("hidden");
  });
}

function modalButtonElements() {
  return Array.from(modalButtons.querySelectorAll("button"));
}

function focusModalButton(index) {
  const btns = modalButtonElements();
  if (!btns.length) return;
  const wrapped = ((index % btns.length) + btns.length) % btns.length;
  btns[wrapped].focus();
}

// Keep focus on the dialog's buttons: Tab/arrows cycle them, Escape cancels
// (or presses the only button).
overlay.addEventListener("keydown", (event) => {
  if (overlay.classList.contains("hidden")) return;
  const btns = modalButtonElements();
  if (!btns.length) return;
  const activeIndex = btns.indexOf(document.activeElement);
  const current = activeIndex === -1 ? 0 : activeIndex;

  if (event.key === "Escape") {
    event.preventDefault();
    const cancelBtn = btns.find((b) => b.textContent === "Cancel");
    (cancelBtn || btns[btns.length - 1]).click();
    return;
  }

  if (event.key === "Tab") {
    event.preventDefault();
    focusModalButton(current + (event.shiftKey ? -1 : 1));
    return;
  }

  if (event.key === "ArrowRight") {
    event.preventDefault();
    focusModalButton(current + 1);
    return;
  }

  if (event.key === "ArrowLeft") {
    event.preventDefault();
    focusModalButton(current - 1);
    return;
  }
});

async function askYesNoCancel(message) {
  const result = await showModal(APP_NAME, message, ["Yes", "No", "Cancel"]);
  return result.toLowerCase();
}

async function showAbout() {
  let webview2Version = "Unknown";
  try {
    const v = await invoke("get_webview2_version");
    if (v) webview2Version = v;
  } catch (e) {}
  let appVersion = "Unknown";
  try {
    const v = await invoke("get_app_version");
    if (v) appVersion = v;
  } catch (e) {}
  const link = document.createElement("a");
  link.href = REPO_URL;
  link.textContent = REPO_URL;
  link.addEventListener("click", (event) => {
    event.preventDefault();
    invoke("open_external_url", { url: REPO_URL }).catch(() => {});
  });
  const body = document.createDocumentFragment();
  body.append(
    `Hawk Version: ${appVersion}\n`,
    link,
    `\n© 2026 Mark West\n\n`,
    `Microsoft Edge WebView2 Version: ${webview2Version}`
  );
  await showModal(APP_NAME, body, ["OK"]);
}

async function showErrorModal(message) {
  await showModal(APP_NAME, message, ["OK"]);
}

// ---------------- File operations ----------------
async function doSave() {
  if (currentPath) {
    try {
      await invoke("write_text_file", { path: currentPath, contents: editor.value, kind: "save" });
      setDirty(false);
      return true;
    } catch (e) {
      await showErrorModal(`Could not save the file:\n${e}`);
      return false;
    }
  }
  return await doSaveAs();
}

async function doSaveAs() {
  let selected;
  try {
    selected = await dialog.save({
      defaultPath: currentPath || lastKnownDir || undefined,
      filters: [{ name: "All Files", extensions: ["*"] }],
    });
  } catch (e) {
    await showErrorModal(`Could not open the save dialog:\n${e}`);
    return false;
  }
  if (!selected) return false;
  try {
    await invoke("write_text_file", { path: selected, contents: editor.value, kind: "save_as" });
    currentPath = selected;
    lastKnownDir = dirOf(selected);
    setDirty(false);
    updateTitle(); // setDirty only does this when the dirty flag changes
    resetPreviewForNewDocument();
    return true;
  } catch (e) {
    await showErrorModal(`Could not save the file:\n${e}`);
    return false;
  }
}

// Returns true if the file was loaded.
async function loadFile(path) {
  let content;
  try {
    content = await invoke("read_text_file", { path });
  } catch (e) {
    await showErrorModal(`Could not open the file:\n${e}`);
    return false;
  }
  editor.value = content;
  currentPath = path;
  lastKnownDir = dirOf(path);
  setDirty(false);
  await updateTitle(); // setDirty only does this when the dirty flag changes
  resetPreviewForNewDocument();
  editor.focus();
  return true;
}

async function confirmProceedForOpen() {
  if (!isDirty) return true;
  const choice = await askYesNoCancel(
    `Do you want to save changes to ${basename(currentPath)}?`
  );
  if (choice === "cancel") return false;
  if (choice === "no") return true;
  if (currentPath) {
    return await doSave();
  }
  // Untitled: Save As, then the user chooses Open again.
  await doSaveAs();
  return false;
}

async function confirmProceedForExit() {
  if (!isDirty) return true;
  const choice = await askYesNoCancel(
    `Do you want to save changes to ${basename(currentPath)}?`
  );
  if (choice === "cancel") return false;
  if (choice === "no") return true;
  return await doSave();
}

async function doOpen() {
  const proceed = await confirmProceedForOpen();
  if (!proceed) return;

  let selected;
  try {
    selected = await dialog.open({
      multiple: false,
      directory: false,
      defaultPath: lastKnownDir || "C:\\",
      filters: [{ name: "All Files", extensions: ["*"] }],
    });
  } catch (e) {
    await showErrorModal(`Could not open the file dialog:\n${e}`);
    return;
  }
  if (!selected) return;
  const path = Array.isArray(selected) ? selected[0] : selected;
  await loadFile(path);
}

async function doNew() {
  try {
    await invoke("spawn_new_instance");
  } catch (e) {
    await showErrorModal(`Could not open a new window:\n${e}`);
  }
}

async function doExit() {
  const proceed = await confirmProceedForExit();
  if (!proceed) return;
  await invoke("exit_app");
}

// ---------------- Operations: copy path / open directory ----------------
async function doCopyPathToClipboard() {
  if (!currentPath) {
    await showErrorModal("This file hasn't been saved yet, so it doesn't have a path to copy.");
    return;
  }
  try {
    await invoke("copy_path_to_clipboard", { path: currentPath });
  } catch (e) {
    await showErrorModal(`Could not copy the file path:\n${e}`);
  }
}

async function doOpenFileDirectory() {
  if (!currentPath) {
    await showErrorModal("This file hasn't been saved yet, so it doesn't have a folder to open.");
    return;
  }
  try {
    await invoke("open_file_directory", { path: currentPath });
  } catch (e) {
    await showErrorModal(`Could not open the file's folder:\n${e}`);
  }
}

// ---------------- Operations: text transforms ----------------
// Transforms are pure text -> text functions. The result is applied with
// execCommand("insertText") so a single Ctrl+Z undoes it; assigning
// editor.value would bypass the undo stack.
function replaceEditorText(newText) {
  if (newText === editor.value) return;
  const scrollPos = editor.scrollTop;
  editor.focus();
  editor.setSelectionRange(0, editor.value.length);
  const applied = document.execCommand("insertText", false, newText);
  if (!applied) {
    // execCommand unsupported: still correct, just not undoable.
    editor.value = newText;
    setDirty(true);
  }
  editor.scrollTop = scrollPos;
}

// Errors (e.g. invalid JSON) are reported in a dialog.
function applyTextOperation(name, transformFn) {
  if (previewOn) return;
  const before = editor.value;
  let after;
  try {
    after = transformFn(before);
  } catch (e) {
    const message = e && e.message ? e.message : String(e);
    showErrorModal(`Could not complete "${name}":\n${message}`);
    invoke("log_event", { message: `Operation FAILED: ${name}: ${message}` }).catch(() => {});
    return;
  }
  if (after === before) {
    invoke("log_event", { message: `Operation: ${name} (no change)` }).catch(() => {});
    return;
  }
  replaceEditorText(after);
  invoke("log_event", { message: `Operation: ${name}` }).catch(() => {});
}

// Format > HTML: a best-effort re-indenter by tag nesting, not a parser.
const HTML_VOID_ELEMENTS = new Set([
  "area", "base", "br", "col", "embed", "hr", "img", "input",
  "link", "meta", "param", "source", "track", "wbr",
]);

function formatHtml(text) {
  const raw = text.match(/<!--[\s\S]*?-->|<!doctype[^>]*>|<[^>]+>|[^<]+/gi) || [];
  const tokens = raw.map((t) => t.trim()).filter((t) => t.length > 0);
  let indent = 0;
  const lines = [];
  let i = 0;
  while (i < tokens.length) {
    const token = tokens[i];
    if (/^<!--/.test(token) || /^<!doctype/i.test(token)) {
      lines.push("  ".repeat(indent) + token);
      i++;
      continue;
    }
    if (/^<\//.test(token)) {
      indent = Math.max(indent - 1, 0);
      lines.push("  ".repeat(indent) + token);
      i++;
      continue;
    }
    if (/^</.test(token)) {
      const nameMatch = token.match(/^<\s*([a-zA-Z0-9-]+)/);
      const tagName = nameMatch ? nameMatch[1].toLowerCase() : "";
      const selfClosing = /\/>$/.test(token) || HTML_VOID_ELEMENTS.has(tagName);
      if (!selfClosing && tagName) {
        // Text-only elements stay on one line: <li>Item</li>.
        const nextText = tokens[i + 1];
        const nextClose = tokens[i + 2];
        const isTextOnly =
          nextText !== undefined &&
          !/^</.test(nextText) &&
          nextClose !== undefined &&
          new RegExp(`^</\\s*${tagName}\\s*>$`, "i").test(nextClose);
        if (isTextOnly) {
          lines.push("  ".repeat(indent) + token + nextText + nextClose);
          i += 3;
          continue;
        }
      }
      lines.push("  ".repeat(indent) + token);
      if (!selfClosing) indent++;
      i++;
      continue;
    }
    lines.push("  ".repeat(indent) + token);
    i++;
  }
  return lines.join("\n");
}

// Format > JSON: 2-space indentation; throws on invalid JSON.
function formatJson(text) {
  return JSON.stringify(JSON.parse(text), null, 2);
}

// Normalizes mixed CRLF/LF/CR first.
function convertLineEndings(text, style) {
  const normalized = text.replace(/\r\n/g, "\n").replace(/\r/g, "\n");
  return style === "windows" ? normalized.replace(/\n/g, "\r\n") : normalized;
}

// Blank Operations
function tabsToSpaces(text) {
  return text.replace(/\t/g, "    ");
}
function trimTrailingSpace(text) {
  return text.split("\n").map((line) => line.replace(/[ \t]+$/, "")).join("\n");
}
function trimLeadingSpace(text) {
  return text.split("\n").map((line) => line.replace(/^[ \t]+/, "")).join("\n");
}
function removeDuplicateSpaces(text) {
  return text.replace(/ {2,}/g, " ");
}
function removeBlankLines(text) {
  return text.split("\n").filter((line) => line.trim() !== "").join("\n");
}

// Code-unit order, not locale collation, so results don't vary by locale.
function sortLines(text, direction) {
  const lines = text.split("\n");
  lines.sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
  if (direction === "desc") lines.reverse();
  return lines.join("\n");
}

// Removes lines identical to the line before (adjacent duplicates only).
function removeDuplicateLines(text) {
  const lines = text.split("\n");
  const result = [];
  for (let i = 0; i < lines.length; i++) {
    if (i === 0 || lines[i] !== lines[i - 1]) result.push(lines[i]);
  }
  return result.join("\n");
}

// ---------------- Wiring ----------------
editor.addEventListener("input", () => setDirty(true));

// Status bar size grip. On Windows the native resize overlay (main.rs) sits
// on top of it and handles the drag; this is the fallback elsewhere.
statusResizeGrip.addEventListener("mousedown", (event) => {
  if (event.button !== 0) return;
  event.preventDefault();
  appWindow.startResizeDragging("SouthEast").catch(() => {});
});

listen("menu:new", doNew);
listen("menu:open", doOpen);
listen("menu:save", doSave);
listen("menu:save-as", doSaveAs);
listen("menu:exit", doExit);
listen("menu:about", showAbout);
listen("menu:word-wrap", (event) => {
  wordWrap = !!event.payload;
  applyWordWrap();
});
listen("menu:spellcheck", (event) => {
  spellcheck = !!event.payload;
  applySpellcheck();
});
listen("menu:tab-mode", (event) => {
  tabMode = event.payload;
});
listen("menu:theme", (event) => {
  theme = event.payload;
  applyTheme();
});
listen("menu:copy-path", doCopyPathToClipboard);
listen("menu:open-dir", doOpenFileDirectory);
listen("menu:format-html", () => applyTextOperation("Format: HTML", formatHtml));
listen("menu:format-json", () => applyTextOperation("Format: JSON", formatJson));
listen("menu:line-endings-windows", () =>
  applyTextOperation("Line Endings: Windows (CR LF)", (t) => convertLineEndings(t, "windows"))
);
listen("menu:line-endings-unix", () =>
  applyTextOperation("Line Endings: Unix (LF)", (t) => convertLineEndings(t, "unix"))
);
listen("menu:tab-to-spaces", () => applyTextOperation("Tab to Spaces", tabsToSpaces));
listen("menu:trim-trailing", () => applyTextOperation("Trim Trailing Space", trimTrailingSpace));
listen("menu:trim-leading", () => applyTextOperation("Trim Leading Space", trimLeadingSpace));
listen("menu:remove-dup-spaces", () =>
  applyTextOperation("Remove All Duplicate Space", removeDuplicateSpaces)
);
listen("menu:remove-blank-lines", () =>
  applyTextOperation("Remove Blank Lines", removeBlankLines)
);
listen("menu:sort-asc", () => applyTextOperation("Sort: Ascending", (t) => sortLines(t, "asc")));
listen("menu:sort-desc", () => applyTextOperation("Sort: Descending", (t) => sortLines(t, "desc")));
listen("menu:remove-dup-lines", () =>
  applyTextOperation("Remove Duplicate Lines", removeDuplicateLines)
);

appWindow.onCloseRequested(async (event) => {
  event.preventDefault();
  await doExit();
});

async function init() {
  let fontError = "";
  try {
    const settings = await invoke("get_settings");
    wordWrap = settings.word_wrap;
    spellcheck = settings.spellcheck;
    tabMode = settings.tab_mode;
    theme = settings.theme;
    currentZoom = typeof settings.zoom === "number" ? settings.zoom : 1.0;
    fontError = applyFont(settings.font);
  } catch (e) {} // keep the defaults
  applyWordWrap();
  applySpellcheck();
  applyTheme();
  updateZoomDisplay();
  setDirty(false);
  resetPreviewForNewDocument();
  let launch = { path: null, preview: false };
  try {
    launch = await invoke("get_launch_args");
  } catch (e) {}
  let loaded = false;
  if (launch.path) {
    loaded = await loadFile(launch.path);
  } else {
    await updateTitle();
  }
  editor.focus();
  // --preview: preview if supported, otherwise a status bar note.
  if (launch.preview) {
    if (loaded && currentDocType !== "") {
      setPreviewMode(true);
    } else if (!launch.path) {
      setStatusInfo("Preview not available: no file was opened.");
    } else if (loaded) {
      setStatusInfo("Preview not available for this file type.");
    }
    // (A failed load has already shown an error dialog.)
  }
  // Last, so a --preview render doesn't clear it; a --preview note wins.
  if (fontError && !statusMessageText.textContent) {
    setStatusError(
      fontError,
      `${fontError}\n\nThe "font" in settings.json isn't installed on this computer. ` +
        `Install it, or change "font" to an installed font and restart Hawk.`
    );
  }
}

init();
