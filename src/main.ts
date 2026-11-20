import { invoke } from "@tauri-apps/api/core";
import { messages, type Language } from "./i18n";
import { codexWorkspaceTag, groupProjects, type ProjectIndex } from "./projects";
import { ScrollFeed } from "./scroll-feed";
import {
  exportFilename, originName, tools,
  type CodeBlock, type Format, type ImageCard, type Page, type Row, type SearchCursor,
  type SearchHit, type Session, type Source, type TextSection, type ToolPart, type ToolRow,
} from "./transcript";

function byId<T extends HTMLElement = HTMLElement>(id: string): T {
  return document.getElementById(id) as T;
}

const toolSelect = byId<HTMLSelectElement>("tool-select");
const list = byId("session-list");
const sessionSearch = byId<HTMLInputElement>("session-search");
const drawer = byId("sidebar");
const toggle = byId<HTMLButtonElement>("sidebar-toggle");
const backdrop = byId("backdrop");
const doc = byId("document");
const toast = byId("toast");
const narrow = matchMedia("(max-width: 759.98px)");

type View = "conversation" | Format;
type ViewContent = Row[] | TextSection[];
type Listing = { sessions: Session[]; warning: string | null };
type Operation = "copy" | "save";
type Feedback = { operation: "copyFailed" | "saveFailed"; error: unknown };

let language: Language = localStorage.getItem("memtrans.language") === "en" ? "en" : "zh";
let source: Source = "codex";
let listing: Listing = { sessions: [], warning: null };
let projects: ProjectIndex = groupProjects([]);
let indexLoading = false;
let indexRequest = 0;
let filteringSessions = false;
// Keep request numbers increasing when the webview reloads in the same app process.
let detailsRequest = Math.floor(performance.timeOrigin * 1000);
let selected: Session | undefined;
let messageCount: number | undefined;
const viewFeeds = new Map<View, ScrollFeed<ViewContent>>();
const toolFeeds = new Map<HTMLDetailsElement, ScrollFeed<ToolPart[]>>();
let mountedView: ScrollFeed<ViewContent> | undefined;
const contentQueries = new Map<View, string>();
let searchResults: ScrollFeed<SearchHit[]> | undefined;
let searchTimer: number | undefined;
const copyTargets = new WeakMap<HTMLButtonElement, CodeBlock>();
const openImages = new Map<HTMLDetailsElement, { url?: string }>();
let readError: unknown;
let readLoading = false;
let view: View = "conversation";
let format: Format = "md";
let drawerOpen = false;
let action: Operation | undefined;
let feedback: Feedback | undefined;
let toastTimer: number | undefined;
let announcement: { kind: "copied" | "copiedCode" } | { kind: "saved"; filename: string } | undefined;
const projectState = new Map<Source, Map<string, boolean>>();

const labels = () => messages[language];
const locale = () => language === "zh" ? "zh-CN" : "en-US";
const owns = (object: object, key: string) => Object.prototype.hasOwnProperty.call(object, key);
const e = (text: string) => text.replace(/[&<>"']/g, (char) => ({
  "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
})[char]!);

function errorText(error: unknown) {
  const errors = labels().errors;
  return typeof error === "string" && owns(errors, error)
    ? errors[error as keyof typeof errors]
    : errors.unknown;
}

function setDrawer(open: boolean, focusToggle = false) {
  drawerOpen = narrow.matches && open;
  drawer.classList.toggle("open", drawerOpen);
  backdrop.hidden = !drawerOpen;
  toggle.setAttribute("aria-expanded", String(drawerOpen));
  toggle.setAttribute("aria-label", drawerOpen ? labels().closeSidebar : labels().openSidebar);
  drawer.inert = narrow.matches && !drawerOpen;
  doc.inert = drawerOpen;
  if (drawerOpen) toolSelect.focus();
  if (focusToggle) toggle.focus();
}

function rememberProjects() {
  if (filteringSessions) return;
  const states = projectState.get(source) ?? new Map<string, boolean>();
  list.querySelectorAll<HTMLDetailsElement>(".project-group").forEach((group) => {
    states.set(group.dataset.project!, group.open);
  });
  projectState.set(source, states);
}

function displayProject(session: Session) {
  return (session.project && projects.pathLabels.get(session.project)) || labels().unassigned;
}

function displayTool(session: Session) {
  return [tools[session.source], displayOrigin(session)].filter(Boolean).join(" · ");
}

function displayOrigin(session: Session) {
  return session.origin === "claudeDesktop" ? labels().desktop : originName(session);
}

function sessionOrigin(session: Session) {
  return session.source === "codex" ? codexWorkspaceTag(session.project) : displayOrigin(session);
}

function displayDate(value: number) {
  const date = new Date(value);
  return value > 0 && Number.isFinite(date.getTime())
    ? new Intl.DateTimeFormat(locale(), { dateStyle: "medium" }).format(date)
    : "";
}

function renderStatic() {
  document.documentElement.lang = language === "zh" ? "zh-CN" : "en";
  byId("language").setAttribute("aria-label", labels().language);
  drawer.setAttribute("aria-label", labels().navigation);
  for (const item of document.querySelectorAll<HTMLButtonElement>("[data-language]")) {
    item.setAttribute("aria-pressed", String(item.dataset.language === language));
  }
  for (const item of document.querySelectorAll<HTMLElement>("[data-label]")) {
    const key = item.dataset.label as "tools" | "projects" | "local";
    item.textContent = labels()[key];
  }
  toolSelect.innerHTML = Object.entries(tools)
    .map(([key, name]) => `<option value="${key}">${name}</option>`).join("");
  toolSelect.value = source;
  sessionSearch.placeholder = labels().searchList;
  sessionSearch.setAttribute("aria-label", labels().searchList);
  toggle.setAttribute("aria-label", drawerOpen ? labels().closeSidebar : labels().openSidebar);
}

function sessionRow(session: Session) {
  const missing = session.project && !session.projectExists;
  const meta = [session.worktree, sessionOrigin(session), displayDate(session.updatedAt)].filter(Boolean);
  return `<button class="session" type="button" aria-current="${selected?.key === session.key}" data-session="${e(session.key)}" title="${e([session.title, session.project].filter(Boolean).join("\n"))}">
    <span class="session-title">${e(session.title)}</span>
    <span class="session-meta">${meta.map((part) => e(part!)).join(" · ")}</span>
    ${missing ? `<span class="missing-project">${labels().missingProject}</span>` : ""}
  </button>`;
}

function renderIndex() {
  const scroll = list.scrollTop;
  const states = projectState.get(source);
  const query = sessionSearch.value.trim().toLocaleLowerCase();
  const matches = (text: string) => text.toLocaleLowerCase().includes(query);
  const grouped = new Map<string, Session[]>();
  for (const session of listing.sessions) {
    const key = (session.project && projects.pathGroups.get(session.project)) || "";
    if (query && ![displayProject(session), session.project ?? "", session.title, session.id, session.worktree ?? "", sessionOrigin(session)].some(matches)) continue;
    const sessions = grouped.get(key) ?? [];
    sessions.push(session);
    grouped.set(key, sessions);
  }

  const notice = indexLoading
    ? `<p class="session-empty" role="status">${labels().indexLoading}</p>`
    : listing.warning
      ? `<div class="index-error" role="status"><strong>${labels().indexError}</strong><p>${e(errorText(listing.warning))}</p><button type="button" id="retry-index" class="ghost">${labels().retry}</button></div>`
      : !grouped.size
        ? `<p class="session-empty">${query ? labels().noMatches : labels().noSessions}</p>`
        : "";

  list.innerHTML = notice + Array.from(grouped, ([key, sessions], index) => {
    const project = projects.groups.find((group) => group.key === key);
    const open = query || (states?.get(key) ?? index === 0);
    return `<details class="project-group" data-project="${e(key)}"${open ? " open" : ""}>
      <summary class="project" title="${e(project?.paths.join("\n") ?? labels().unassigned)}">
        <span class="project-arrow" aria-hidden="true">›</span>
        <span class="project-name">${e(project?.name ?? labels().unassigned)}</span>
        <span class="project-count">${sessions.length}</span>
      </summary>
      <div class="group-sessions">${sessions.map(sessionRow).join("")}</div>
    </details>`;
  }).join("");
  list.scrollTop = scroll;
}

async function loadIndex() {
  const request = ++indexRequest;
  const requestedSource = source;
  rememberProjects();
  indexLoading = true;
  renderIndex();
  try {
    const result = await invoke<Listing>("list_sessions", { source: requestedSource });
    if (request !== indexRequest) return;
    listing = result;
    projects = groupProjects(result.sessions);
  } catch (error) {
    if (request !== indexRequest) return;
    const code = typeof error === "string" ? error : "unknown";
    listing = { ...listing, warning: code };
  }
  rememberProjects();
  indexLoading = false;
  renderIndex();
  if (selected) renderDocument(true);
}

function clearFeedback() {
  feedback = undefined;
  announcement = undefined;
  window.clearTimeout(toastTimer);
  renderToast();
}

function renderToast() {
  toast.textContent = announcement
    ? announcement.kind === "saved" ? labels().saved + announcement.filename : labels()[announcement.kind]
    : "";
  toast.classList.toggle("show", Boolean(announcement));
}

function announce(value: NonNullable<typeof announcement>) {
  announcement = value;
  renderToast();
  toastTimer = window.setTimeout(() => {
    announcement = undefined;
    renderToast();
  }, 3600);
}

function markSelected() {
  for (const row of list.querySelectorAll<HTMLButtonElement>("[data-session]")) {
    row.setAttribute("aria-current", String(row.dataset.session === selected?.key));
  }
}

function resetViews() {
  window.clearTimeout(searchTimer);
  contentQueries.clear();
  searchResults?.dispose();
  searchResults = undefined;
  for (const feed of viewFeeds.values()) feed.dispose();
  for (const feed of toolFeeds.values()) feed.dispose();
  for (const image of openImages.values()) {
    if (image.url) URL.revokeObjectURL(image.url);
  }
  viewFeeds.clear();
  toolFeeds.clear();
  openImages.clear();
  mountedView = undefined;
  messageCount = undefined;
}

async function read(session: Session) {
  const request = ++detailsRequest;
  selected = session;
  resetViews();
  readError = undefined;
  readLoading = true;
  clearFeedback();
  markSelected();
  renderDocument();
  if (drawerOpen) {
    setDrawer(false);
    doc.focus();
  }
  try {
    const count = await invoke<number>("read_session", { source: session.source, key: session.key, request });
    if (request !== detailsRequest) return;
    messageCount = count;
  } catch (error) {
    if (request !== detailsRequest) return;
    readError = error ?? "unknown";
  }
  readLoading = false;
  renderDocument();
}

function calendarDay(time: string | null) {
  return time?.slice(0, 10) ?? "";
}

function dayLabel(day: string) {
  return new Intl.DateTimeFormat(locale(), {
    dateStyle: "long", timeZone: "UTC",
  }).format(new Date(`${day}T00:00:00Z`));
}

function timeLabel(time: string | null) {
  return time
    ? ` <time datetime="${e(time)}" title="${e(time)}">${e(time.slice(11, 16))}</time>`
    : "";
}

function toolPanel(entry: ToolRow) {
  const name = entry.name ? e(entry.name) : feedLabel("toolResult");
  return `<details class="tool-card" data-tool-card="${entry.index}">
    <summary><span aria-hidden="true" class="tool-card-icon">⌘</span>
      <span class="tool-card-name">${name}</span>
      ${entry.hint === null ? "" : `<span class="tool-card-hint">${e(entry.hint)}</span>`}
    </summary>
    <div class="tool-card-body">
      <div class="tool-content"></div>
      ${entry.callId ? `<span class="tool-id">${e(entry.callId)}</span>` : ""}
    </div>
  </details>`;
}

const formats: [Format, string][] = [["md", "Markdown"], ["jsonl", "JSONL"]];
const views: View[] = ["conversation", "md", "jsonl"];

const spinner = '<span class="spinner" aria-hidden="true"></span>';

function loading(text: string) {
  return `<p class="loading" role="status">${spinner}${text}</p>`;
}

type FeedLabel = "user" | "noMessages" | "toolResult" | "command" | "output"
  | "imageAttachment" | "previewImage" | "imageUnavailable" | "imageLoading" | "imageError";

function feedLabel(key: FeedLabel) {
  return `<span data-feed-label="${key}">${labels()[key]}</span>`;
}

function localizeFeed(root: HTMLElement) {
  for (const item of root.querySelectorAll<HTMLElement>("[data-feed-label]")) {
    item.textContent = labels()[item.dataset.feedLabel as FeedLabel];
  }
  for (const button of root.querySelectorAll<HTMLButtonElement>("[data-code-copy]")) {
    button.title = labels().copyCode;
    button.setAttribute("aria-label", labels().copyCode);
  }
  for (const item of root.querySelectorAll<HTMLImageElement>("img[data-generic-alt]")) {
    item.alt = labels().imageAttachment;
  }
  for (const item of root.querySelectorAll<HTMLElement>("[data-day]")) {
    item.textContent = dayLabel(item.dataset.day!);
  }
}

function scrollElement(className: string) {
  const element = document.createElement("div");
  element.className = className;
  element.tabIndex = 0;
  return element;
}

function updateChatHeader(header: HTMLElement, session: Session) {
  const info = [displayTool(session), displayProject(session), session.worktree].filter(Boolean).join(" · ");
  header.innerHTML = `<div class="chat-eyebrow">${e(info)}</div>
    <h1 class="chat-title">${e(session.title)}</h1>
    <div class="chat-meta">${e([displayDate(session.updatedAt), labels().messageCount(messageCount!)].filter(Boolean).join(" · "))}</div>`;
}

function appendWrappedText(element: HTMLElement, text: string) {
  if (!/(?:^|\s)\S{512}/u.test(text)) {
    element.append(text);
    return;
  }
  // Add wrap opportunities to unusually long tokens while keeping the actual text.
  let start = 0;
  for (const token of text.matchAll(/\S+/gu)) {
    if (token[0].length < 512) continue;
    element.append(text.slice(start, token.index));
    let first = true;
    for (const piece of token[0].matchAll(/.{1,128}/gu)) {
      if (!first) element.append(document.createElement("wbr"));
      element.append(piece[0]);
      first = false;
    }
    start = token.index + token[0].length;
  }
  element.append(text.slice(start));
}

const copyGlyph = '<span class="copy-glyph" aria-hidden="true"></span>';

function codeBlock(target: CodeBlock, label?: "command" | "output") {
  const block = document.createElement("div");
  block.className = "code-block";
  const actions = document.createElement("div");
  actions.className = "code-actions";
  if (label) actions.innerHTML = `<span class="tool-label">${feedLabel(label)}</span>`;
  const button = document.createElement("button");
  button.type = "button";
  button.className = "copy-icon";
  button.dataset.codeCopy = "";
  button.setAttribute("aria-label", labels().copyCode);
  button.title = labels().copyCode;
  button.innerHTML = copyGlyph;
  copyTargets.set(button, target);
  actions.append(button);
  block.append(actions);
  return block;
}

function appendCodeText(parent: HTMLElement, text: string, wrap = true) {
  const pre = document.createElement("pre");
  const code = document.createElement("code");
  if (wrap) {
    pre.className = "raw-pre";
    appendWrappedText(code, text);
  } else {
    pre.tabIndex = 0;
    code.textContent = text;
  }
  pre.append(code);
  parent.append(pre);
}

function sectionWriter(host: HTMLElement, target: (block: number) => CodeBlock, raw = false, tail: ChildNode | null = null) {
  let last: { index: number; element: HTMLElement } | undefined;
  return (sections: TextSection[]) => {
    for (const part of sections) {
      if (part.block === undefined) {
        last = undefined;
        if (raw) {
          const pre = document.createElement("pre");
          pre.className = "raw-pre";
          appendWrappedText(pre, part.text);
          host.insertBefore(pre, tail);
        } else {
          appendWrappedText(host, part.text);
        }
        continue;
      }
      if (last?.index !== part.block) {
        const element = codeBlock(target(part.block));
        host.insertBefore(element, tail);
        last = { index: part.block, element };
      }
      appendCodeText(last.element, part.text);
    }
  };
}

function imageCard(image: ImageCard, index: number, message: number) {
  const name = image.name ? e(image.name) : feedLabel("imageAttachment");
  const title = `<span class="image-mark" aria-hidden="true"></span><span class="image-title">${name}</span>`;
  if (!image.mime) {
    return `<div class="image-card image-unavailable">${title}<span class="image-action">${feedLabel("imageUnavailable")}</span></div>`;
  }
  return `<details class="image-card" data-image="${index}" data-message="${message}" data-mime="${e(image.mime)}">
    <summary>${title}<span class="image-action">${feedLabel("previewImage")}</span><span class="image-arrow" aria-hidden="true">›</span></summary>
    <div class="image-preview"></div>
  </details>`;
}

function closeImage(card: HTMLDetailsElement) {
  const pending = openImages.get(card);
  if (pending?.url) URL.revokeObjectURL(pending.url);
  openImages.delete(card);
  card.querySelector(".image-preview")!.replaceChildren();
}

async function loadImage(card: HTMLDetailsElement) {
  if (openImages.has(card)) return;
  const request = detailsRequest;
  const target = card.querySelector<HTMLElement>(".image-preview")!;
  const pending: { url?: string } = {};
  openImages.set(card, pending);
  target.innerHTML = `<p class="loading" role="status">${spinner}${feedLabel("imageLoading")}</p>`;
  const active = () => request === detailsRequest && card.open && openImages.get(card) === pending;
  const failed = () => {
    if (!active()) return;
    if (pending.url) URL.revokeObjectURL(pending.url);
    pending.url = undefined;
    target.innerHTML = `<p class="image-error" role="status">${feedLabel("imageError")}</p>`;
  };
  try {
    const bytes = await invoke<ArrayBuffer | number[]>("read_image", {
      request, index: Number(card.dataset.message), image: Number(card.dataset.image),
    });
    if (!active()) return;
    const image = document.createElement("img");
    const title = card.querySelector(".image-title")!;
    image.alt = title.textContent || labels().imageAttachment;
    if (title.querySelector("[data-feed-label]")) image.dataset.genericAlt = "";
    image.decoding = "async";
    image.addEventListener("load", () => {
      if (active()) target.replaceChildren(image);
    }, { once: true });
    image.addEventListener("error", failed, { once: true });
    const data = Array.isArray(bytes) ? Uint8Array.from(bytes) : new Uint8Array(bytes);
    pending.url = URL.createObjectURL(new Blob([data], { type: card.dataset.mime }));
    image.src = pending.url;
  } catch {
    failed();
  }
}

function conversationContent(scroll: HTMLElement, session: Session) {
  scroll.innerHTML = '<div class="chat"><header class="chat-head"></header><div class="msgs"></div></div>';
  const items = scroll.querySelector<HTMLElement>(".msgs")!;
  const toolName = tools[session.source];
  let previousDay = "";
  let lastMessage: { index: number; append: (sections: TextSection[]) => void } | undefined;

  return (rows: Row[]) => {
    if (!rows.length) {
      items.innerHTML = `<p class="chat-meta">${feedLabel("noMessages")}</p>`;
      return;
    }
    const batch = document.createDocumentFragment();
    for (const entry of rows) {
      if (entry.kind === "message" && entry.continued && lastMessage?.index === entry.index) {
        lastMessage.append(entry.text);
        continue;
      }
      const day = calendarDay(entry.time);
      if (day && day !== previousDay) {
        const separator = document.createElement("div");
        separator.className = "chat-day";
        separator.dataset.day = day;
        separator.textContent = dayLabel(day);
        batch.append(separator);
      }
      if (day) previousDay = day;
      const user = entry.kind === "message" && entry.role === "user";
      const article = document.createElement("article");
      article.className = user ? "msg user" : entry.kind === "tool" ? "msg assistant with-tool" : "msg assistant";
      const body = entry.kind === "tool" ? toolPanel(entry) : `<div class="bubble">
        <div class="bubble-text"></div>${entry.images?.length
          ? `<div class="message-images">${entry.images.map((image, i) => imageCard(image, i, entry.index)).join("")}</div>`
          : ""}
      </div>`;
      article.innerHTML = `${user ? "" : `<div class="avatar ${session.source}" aria-hidden="true">${e(toolName[0])}</div>`}
        <div class="msg-body"><div class="msg-meta">${user ? feedLabel("user") : e(toolName)}${timeLabel(entry.time)}</div>${body}</div>`;
      if (entry.kind === "message") {
        const element = article.querySelector<HTMLElement>(".bubble-text")!;
        const append = sectionWriter(element, (block) => ({ kind: "message", index: entry.index, block }));
        append(entry.text);
        lastMessage = { index: entry.index, append };
      } else {
        lastMessage = undefined;
      }
      batch.append(article);
    }
    items.append(batch);
  };
}

function getView(view: View, session: Session) {
  const existing = viewFeeds.get(view);
  if (existing) return existing;
  const request = detailsRequest;
  const scroll = scrollElement(view === "conversation" ? "chat-scroll" : "raw-scroll");
  scroll.id = "document-scroll";
  const appendRows = view === "conversation" ? conversationContent(scroll, session) : undefined;
  let appendSections: ((sections: TextSection[]) => void) | undefined;
  const feed = new ScrollFeed<ViewContent>(scroll, {
    read: (page) => invoke<Page<ViewContent>>("read_view", { request, view, page }),
    append: (content) => {
      if (appendRows) {
        appendRows(content as Row[]);
      } else {
        appendSections ??= sectionWriter(scroll, (block) => ({ kind: "markdown", block }), true, scroll.lastChild);
        appendSections(content as TextSection[]);
      }
    },
    loading: () => labels().viewLoading,
    error: (error) => `${labels().viewError} · ${errorText(error)}`,
    retry: () => labels().retry,
  });
  viewFeeds.set(view, feed);
  return feed;
}

function openTool(card: HTMLDetailsElement) {
  if (toolFeeds.has(card)) return;
  const request = detailsRequest;
  const index = Number(card.dataset.toolCard);
  const scroll = scrollElement("tool-scroll");
  let last: { index: number; element: HTMLElement } | undefined;
  const feed = new ScrollFeed<ToolPart[]>(scroll, {
    read: (page) => invoke<Page<ToolPart[]>>("read_tool", { request, index, page }),
    append: (parts) => {
      for (const { index, command, text } of parts) {
        if (last?.index !== index) {
          const line = document.createElement("div");
          line.className = "tool-line";
          const element = codeBlock({ kind: "tool", index }, command ? "command" : "output");
          line.append(element);
          scroll.lastElementChild!.before(line);
          last = { index, element };
        }
        appendCodeText(last.element, text, false);
      }
    },
    loading: () => labels().toolLoading,
    error: errorText,
    retry: () => labels().retry,
  });
  toolFeeds.set(card, feed);
  feed.mount(card.querySelector<HTMLElement>(".tool-content")!);
}

function searchDescription() {
  return view === "conversation" ? labels().searchConversation
    : labels().searchSource(view === "md" ? "Markdown" : "JSONL");
}

function renderContent() {
  searchResults?.dispose();
  searchResults?.element.remove();
  searchResults = undefined;
  if (!selected) return;
  const panel = byId("doc-body");
  const query = contentQueries.get(view)?.trim();
  if (!query) {
    if (mountedView) return;
    mountedView = getView(view, selected);
    const header = mountedView.element.querySelector<HTMLElement>(".chat-head");
    if (header) updateChatHeader(header, selected);
    mountedView.mount(panel);
    for (const feed of toolFeeds.values()) feed.observe();
    return;
  }
  mountedView?.unmount();
  mountedView = undefined;
  const request = detailsRequest;
  const requestedView = view;
  const scroll = scrollElement(view === "conversation" ? "search-results" : "search-results raw-results");
  let cursor: SearchCursor | null = null;
  searchResults = new ScrollFeed<SearchHit[]>(scroll, {
    read: async (page) => {
      const result = await invoke<{ hits: SearchHit[]; next: SearchCursor | null }>("search_view", {
        request, view: requestedView, query, cursor,
      });
      cursor = result.next;
      return { content: result.hits, total: cursor ? page + 2 : page + 1 };
    },
    append: (hits) => {
      const html = hits.length ? hits.map((hit) => `<article class="search-hit">
        ${hit.kind === "source" ? "" : `<div class="search-hit-meta">${labels()[hit.kind]}</div>`}
        <div class="search-hit-text">${e(hit.before)}<mark>${e(hit.matched)}</mark>${e(hit.after)}</div>
      </article>`).join("") : `<p class="session-empty" role="status">${labels().noMatches}</p>`;
      scroll.lastElementChild!.insertAdjacentHTML("beforebegin", html);
    },
    loading: () => labels().searching,
    error: (error) => `${labels().searchFailed} · ${errorText(error)}`,
    retry: () => labels().retry,
  });
  searchResults.mount(panel);
}

function renderDocument(preserve = false) {
  const active = preserve && doc.contains(document.activeElement) ? (document.activeElement as HTMLElement).id : "";
  window.clearTimeout(searchTimer);
  searchResults?.dispose();
  searchResults = undefined;
  mountedView?.unmount();
  mountedView = undefined;
  if (!selected) {
    doc.innerHTML = `<div class="empty-doc">
      <div class="empty-icon" aria-hidden="true">▤</div>
      <h1>${labels().select}</h1>
      <p>${labels().selectHint}</p>
      <button class="primary only-narrow" id="open-list" type="button">${labels().openSidebar}</button>
    </div>`;
    return;
  }

  const session = selected;
  const actions = messageCount !== undefined ? `<div class="doc-actions">
    <select id="export-format" class="ghost" aria-label="${labels().format}">${formats.map(([key, label]) =>
      `<option value="${key}"${key === format ? " selected" : ""}>${label}</option>`).join("")}</select>
    <button type="button" id="copy-source" class="ghost"></button>
    <button class="primary" id="export-btn" type="button"></button>
  </div>` : "";
  const toolbar = `<div class="doc-toolbar">
    <div class="crumbs">
      <span>${e(displayTool(session))}</span>
      <span class="crumb-sep" aria-hidden="true">/</span>
      <span class="crumb-project" title="${e(session.project ?? "")}">${e(displayProject(session))}</span>
      <span class="crumb-sep project-sep" aria-hidden="true">/</span>
      <strong title="${e(session.title)}">${e(session.title)}</strong>
    </div>${actions}
  </div>`;

  if (messageCount === undefined) {
    const body = readLoading
      ? loading(labels().readLoading)
      : `<div class="error-box" role="status"><h1>${labels().readError}</h1>
        <p>${e(errorText(readError))}</p>
        <button class="ghost" id="retry-read" type="button">${labels().retry}</button>
      </div>`;
    doc.innerHTML = toolbar + `<div class="doc-body load-body">${body}</div>`;
    return;
  }
  const viewbar = `<div class="view-bar">
    <div class="tabs" role="tablist" aria-label="${labels().views}">${views.map((item) =>
      `<button class="tab" type="button" role="tab" id="tab-${item}" aria-controls="doc-body"
        aria-selected="${view === item}" tabindex="${view === item ? 0 : -1}" data-view="${item}">${item === "conversation" ? labels().conversation : formats.find(([key]) => key === item)![1]}</button>`).join("")}</div>
    <input class="search-input" id="content-search" type="search" maxlength="180" aria-label="${e(searchDescription())}"
      aria-controls="doc-body" placeholder="${e(searchDescription())}" value="${e(contentQueries.get(view) ?? "")}">
  </div>`;
  const sourceActions = view === "conversation" ? "" : `<div class="source-actions">
    <button type="button" id="copy-source-icon" class="copy-icon" title="${labels().copy}" aria-label="${labels().copy}">${copyGlyph}</button>
  </div>`;
  doc.innerHTML = toolbar + viewbar + sourceActions + '<div class="action-error" id="action-error" role="alert" hidden></div>'
    + '<span class="visually-hidden" id="action-status" role="status"></span>'
    + `<div id="doc-body" class="doc-body" role="tabpanel" tabindex="-1" aria-labelledby="tab-${view}"></div>`
    + `<div class="doc-foot"><span>${labels().local}</span><span class="session-id" title="${e(session.id)}">${e(session.id)}</span></div>`;
  renderActions();
  renderContent();
  if (active) document.getElementById(active)?.focus({ preventScroll: true });
}

function renderActions() {
  const controls = [
    ["export-btn", "save", labels().export, labels().saving],
    ["copy-source", "copy", labels().copy, labels().copying],
  ] as const;
  for (const [id, operation, idle, running] of controls) {
    const button = document.getElementById(id) as HTMLButtonElement | null;
    if (!button) continue;
    button.disabled = action !== undefined;
    button.innerHTML = action === operation ? spinner + running : idle;
  }
  const live = document.getElementById("action-status");
  const operationLabel = action === "save" ? labels().saving : action === "copy" ? labels().copying : "";
  if (live && live.textContent !== operationLabel) live.textContent = operationLabel;
  const status = document.getElementById("action-error");
  if (status) {
    status.hidden = !feedback;
    status.innerHTML = feedback
      ? `<strong>${labels()[feedback.operation]}</strong> ${e(errorText(feedback.error))}`
      : "";
  }
}

async function doAction(operation: Operation, copyTarget?: CodeBlock | Format) {
  if (!selected || messageCount === undefined || action) return;
  const request = detailsRequest;
  const args = { request, format: typeof copyTarget === "string" ? copyTarget : format };
  const block = typeof copyTarget === "object" ? copyTarget : undefined;
  const title = selected.title;
  action = operation;
  clearFeedback();
  renderActions();
  try {
    if (operation === "copy") {
      if (block) await invoke("copy_block", { request, target: block });
      else await invoke("copy_source", args);
      if (request === detailsRequest) announce({ kind: block ? "copiedCode" : "copied" });
    } else {
      const filename = await invoke<string | null>("save_export", {
        ...args,
        filename: exportFilename(title, args.format),
        label: labels().export,
      });
      if (filename !== null && request === detailsRequest) announce({ kind: "saved", filename });
    }
  } catch (error) {
    if (request === detailsRequest) {
      feedback = { operation: operation === "copy" ? "copyFailed" : "saveFailed", error };
    }
  } finally {
    action = undefined;
    renderActions();
    if (request === detailsRequest) {
      if (document.activeElement === document.body) {
        document.getElementById(operation === "copy" ? "copy-source" : "export-btn")?.focus();
      }
    }
  }
}

function openView(next: View) {
  if (view === next) return;
  view = next;
  renderDocument();
  byId(`tab-${view}`).focus();
}

byId("language").addEventListener("click", (event) => {
  const button = (event.target as HTMLElement).closest<HTMLButtonElement>("[data-language]");
  if (!button || button.dataset.language === language) return;
  rememberProjects();
  language = button.dataset.language as Language;
  localStorage.setItem("memtrans.language", language);
  renderStatic();
  renderIndex();
  for (const feed of viewFeeds.values()) {
    localizeFeed(feed.element);
    feed.refreshStatus();
  }
  for (const feed of toolFeeds.values()) feed.refreshStatus();
  renderDocument(true);
  renderToast();
});

toolSelect.addEventListener("change", () => {
  const next = toolSelect.value as Source;
  if (!owns(tools, next) || next === source) return;
  rememberProjects();
  source = next;
  ++detailsRequest;
  listing = { sessions: [], warning: null };
  projects = groupProjects([]);
  selected = undefined;
  resetViews();
  clearFeedback();
  renderDocument();
  void loadIndex();
});

sessionSearch.addEventListener("input", () => {
  const filtering = Boolean(sessionSearch.value.trim());
  if (filtering && !filteringSessions) rememberProjects();
  filteringSessions = filtering;
  renderIndex();
});

doc.addEventListener("input", (event) => {
  const input = event.target as HTMLInputElement;
  if (input.id !== "content-search") return;
  contentQueries.set(view, input.value);
  window.clearTimeout(searchTimer);
  searchTimer = window.setTimeout(renderContent, input.value.trim() ? 180 : 0);
});

list.addEventListener("click", (event) => {
  const target = event.target as HTMLElement;
  const button = target.closest<HTMLButtonElement>("[data-session]");
  if (button) {
    const session = listing.sessions.find((session) => session.key === button.dataset.session);
    if (!session) return;
    if (selected?.key === session.key && (messageCount !== undefined || readLoading)) {
      if (drawerOpen) {
        setDrawer(false);
        doc.focus();
      }
      return;
    }
    view = "conversation";
    format = "md";
    void read(session);
  } else if (target.id === "retry-index") {
    void loadIndex();
  }
});

doc.addEventListener("click", (event) => {
  const target = (event.target as HTMLElement).closest<HTMLButtonElement>("button");
  if (!target) return;
  const block = copyTargets.get(target);
  if (block) {
    void doAction("copy", block);
  } else if (target.dataset.view) {
    openView(target.dataset.view as View);
  } else if (target.id === "open-list") {
    setDrawer(true);
  } else if (target.id === "retry-read" && selected) {
    const session = selected;
    void read(session);
    doc.focus();
  } else if (target.id === "copy-source-icon" && view !== "conversation") {
    void doAction("copy", view);
  } else if (target.id === "export-btn" || target.id === "copy-source") {
    void doAction(target.id === "export-btn" ? "save" : "copy");
  }
});

doc.addEventListener("toggle", (event) => {
  const card = event.target;
  if (!(card instanceof HTMLDetailsElement)) return;
  if (card.matches(".image-card")) {
    if (card.open) void loadImage(card);
    else closeImage(card);
  } else if (card.matches(".tool-card")) {
    if (card.open) openTool(card);
    else {
      toolFeeds.get(card)?.dispose();
      toolFeeds.delete(card);
      card.querySelector(".tool-content")!.replaceChildren();
    }
  }
}, true);

doc.addEventListener("change", (event) => {
  const target = event.target as HTMLSelectElement;
  if (target.id !== "export-format") return;
  format = target.value as Format;
});

doc.addEventListener("keydown", (event) => {
  const target = event.target as HTMLElement;
  if (!target.matches("[data-view]")) return;
  const index = views.indexOf(target.dataset.view as View);
  const positions: Record<string, number> = {
    ArrowRight: (index + 1) % views.length,
    ArrowLeft: (index + views.length - 1) % views.length,
    Home: 0,
    End: views.length - 1,
  };
  if (!owns(positions, event.key)) return;
  event.preventDefault();
  openView(views[positions[event.key]]);
});

toggle.addEventListener("click", () => setDrawer(!drawerOpen));
backdrop.addEventListener("click", () => setDrawer(false, true));
document.addEventListener("keydown", (event) => {
  if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "f") {
    const input = drawerOpen ? sessionSearch : document.getElementById("content-search") as HTMLInputElement | null;
    if (input || !narrow.matches) {
      event.preventDefault();
      (input ?? sessionSearch).focus();
      (input ?? sessionSearch).select();
    }
  }
  if (event.key === "Escape" && drawerOpen && document.activeElement !== sessionSearch) setDrawer(false, true);
});
narrow.addEventListener("change", () => setDrawer(false));

renderStatic();
setDrawer(false);
renderDocument();
void loadIndex();
