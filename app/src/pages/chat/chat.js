// Message content is server HTML; clicks, context menus and copy inside it are delegated here and
// handed to Rust.
(() => {
  if (window.zulipChat) return;
  const panes = new Map();

  const paneOf = (node) => {
    const el = node instanceof Element ? node : node && node.parentElement;
    if (!el) return null;
    for (const [id, send] of panes) {
      if (el.closest(`#${id}`)) return send;
    }
    return null;
  };
  const msgOf = (el) => {
    const row = el.closest("[data-msg]");
    return row ? Number(row.dataset.msg) : null;
  };

  const serialize = (fragment) => {
    const box = document.createElement("div");
    box.appendChild(fragment);
    return box.innerHTML;
  };

  // Selected content: when it spans several messages, each is prefixed with "name: time", with a
  // blank line between messages
  const selectionPayload = (sel) => {
    const range = sel.getRangeAt(0);
    const start = range.startContainer;
    const root = (start instanceof Element ? start : start.parentElement).closest(".chat-list");
    const touched = root
      ? [...root.querySelectorAll("[data-msg]")].filter((r) => range.intersectsNode(r))
      : [];
    // System notices are not selectable: a range dragged across them must not copy their text
    const rows = touched.filter((r) => !r.classList.contains("chat-notice"));
    if (touched.length < 2) {
      return { text: sel.toString(), html: serialize(range.cloneContents()) };
    }
    const clip = (row) => {
      const part = range.cloneRange();
      if (row.contains(range.startContainer)) part.setEndAfter(row);
      else if (row.contains(range.endContainer)) part.setStartBefore(row);
      else part.selectNode(row);
      return part;
    };
    if (rows.length === 1) {
      const box = document.createElement("div");
      box.appendChild(clip(rows[0]).cloneContents());
      return { text: box.innerText, html: box.innerHTML };
    }
    const texts = [];
    const htmls = [];
    for (const row of rows) {
      const part = clip(row);
      const box = document.createElement("div");
      box.appendChild(part.cloneContents());
      box.querySelectorAll(".msg-avatar, .msg-name, .msg-time, .msg-reactions, .table-tools").forEach((e) => e.remove());
      const content = box.innerText.trim();
      if (!content) continue;
      const head = row.dataset.head;
      texts.push(head ? `${head}\n${content}` : content);
      htmls.push(box.innerHTML);
    }
    return { text: texts.join("\n\n"), html: htmls.join("") };
  };

  // Intercept in the capture phase: links must not reach the framework's default open behavior
  const onClick = (e) => {
    const send = paneOf(e.target);
    if (!send || !(e.target instanceof Element)) return;
    const t = e.target;
    const content = t.closest(".msg-content, .msg-annotation-text");
    if (!content) return;
    const consume = () => {
      e.preventDefault();
      e.stopPropagation();
    };
    // A channel announcement belongs to no message; the event carries the channel id
    const notice = t.closest("[data-notice]");
    const noticeStream = notice ? Number(notice.dataset.notice) : null;
    // In multi-select, a click only toggles the message. The event cannot be intercepted here (the
    // message row must receive the click); CSS lets clicks pass through links
    if (!notice && t.closest(".chat-pane.multi")) return;
    const more = t.closest(".table-more");
    if (more) {
      consume();
      const index = Number(more.closest(".table-host").dataset.table);
      const at = { index, x: e.clientX, y: e.clientY };
      send(notice ? { t: "noticetable", stream: noticeStream, ...at } : { t: "table", msg: msgOf(t), ...at });
      return;
    }
    const spoiler = t.closest(".spoiler-head");
    if (spoiler) {
      consume();
      spoiler.parentElement.classList.toggle("open");
      return;
    }
    // Images open on double click (onDblClick); a single click does nothing
    if (t.closest(".img-preview")) {
      consume();
      return;
    }
    const mention = t.closest(".mention");
    if (mention) {
      consume();
      const d = mention.dataset;
      send({
        t: "mention",
        user: d.userId || null,
        group: d.userGroupId || null,
        wildcard: d.wildcard || null,
        text: mention.textContent,
        x: e.clientX,
        y: e.clientY,
      });
      return;
    }
    const a = t.closest("a[href]");
    if (a) {
      consume();
      send({ t: "link", msg: msgOf(t), href: a.getAttribute("href") });
      return;
    }
    // Links and mentions inside quotes are handled first; no navigation if text was selected by
    // dragging
    const quote = t.closest(".quote[data-href]");
    if (quote && document.getSelection().isCollapsed) {
      consume();
      send({ t: "link", msg: msgOf(t), href: quote.dataset.href });
    }
  };

  const onDblClick = (e) => {
    const send = paneOf(e.target);
    if (!send || !(e.target instanceof Element)) return;
    const t = e.target;
    const image = t.closest(".msg-content .img-preview, .msg-annotation-text .img-preview");
    if (!image) return;
    const notice = t.closest("[data-notice]");
    if (!notice && t.closest(".chat-pane.multi")) return;
    e.preventDefault();
    e.stopPropagation();
    const index = Number(image.dataset.index);
    send(notice ? { t: "noticeimage", stream: Number(notice.dataset.notice), index } : { t: "image", msg: msgOf(t), index });
  };

  // Image downloaded but cannot be displayed (corrupt file, unsupported format): show the
  // placeholder box as for a failed download, without a broken-image icon
  const onImageError = (e) => {
    const img = e.target;
    if (!(img instanceof HTMLImageElement) || !img.getAttribute("src")) return;
    const box = img.closest(".img-preview");
    if (box) box.classList.add("failed");
  };

  // Table column widths: the table fills the available width, distributed in proportion to the
  // longest content of each column (content that does not fit wraps automatically), each column at
  // least TABLE_MIN_COLUMN; if it still does not fit with every column at the minimum, scroll
  // horizontally
  const TABLE_MIN_COLUMN = 35;
  const fitColumns = (natural, available) => {
    const fixed = new Set();
    for (;;) {
      const open = natural.map((_, i) => i).filter((i) => !fixed.has(i));
      if (!open.length) return natural.map(() => TABLE_MIN_COLUMN);
      const room = available - fixed.size * TABLE_MIN_COLUMN;
      const sum = open.reduce((a, i) => a + natural[i], 0);
      const narrow = open.filter((i) => (natural[i] * room) / sum < TABLE_MIN_COLUMN);
      if (!narrow.length) return natural.map((w, i) => (fixed.has(i) ? TABLE_MIN_COLUMN : (w * room) / sum));
      narrow.forEach((i) => fixed.add(i));
    }
  };
  const layoutTable = (host) => {
    const table = host.querySelector(":scope > .table-scroll > table");
    const first = table && table.rows[0];
    if (!first || [...table.rows].some((r) => [...r.cells].some((c) => c.colSpan > 1))) return;
    table.querySelector(":scope > colgroup")?.remove();
    table.classList.remove("fitted");
    table.style.width = "";
    // Without wrapping, each column's width is the width of its longest content
    table.classList.add("measuring");
    const natural = [...first.cells].map((c) => c.getBoundingClientRect().width);
    // When the table fills its space, the outer containers (bubble, quote) are all at max width, so
    // the table area's width is the available width
    table.style.width = "100000px";
    const available = host.getBoundingClientRect().width;
    table.classList.remove("measuring");
    // With collapsed borders the table is half an outer border wider than the sum of the columns;
    // leave 1px to avoid a horizontal scrollbar
    const widths = fitColumns(natural, Math.floor(available) - 1);
    const group = document.createElement("colgroup");
    for (const w of widths) {
      const col = document.createElement("col");
      col.style.width = `${w}px`;
      group.appendChild(col);
    }
    table.prepend(group);
    table.style.width = `${widths.reduce((a, b) => a + b, 0)}px`;
    table.classList.add("fitted");
  };
  // Recompute when the width of a quote in a message row or the composer changes (window size,
  // sidebar); height-only changes are ignored
  const tableWidths = new WeakMap();
  const tableResize = new ResizeObserver((entries) => {
    for (const entry of entries) {
      // Also called back when removed from the page (virtual list recycling): stop observing
      if (!entry.target.isConnected) {
        tableResize.unobserve(entry.target);
        continue;
      }
      const width = Math.round(entry.target.getBoundingClientRect().width);
      if (tableWidths.get(entry.target) === width) continue;
      tableWidths.set(entry.target, width);
      entry.target.querySelectorAll(".table-host").forEach(layoutTable);
    }
  });
  const watchTables = (root) => {
    const hosts = root.matches(".table-host") ? [root] : root.querySelectorAll(".table-host");
    for (const host of hosts) {
      layoutTable(host);
      const box = host.closest(".msg-row, .zq") || host.parentElement;
      // Also called back on the first observation: just record the width, do not recompute
      if (!tableWidths.has(box)) tableWidths.set(box, Math.round(box.getBoundingClientRect().width));
      tableResize.observe(box);
    }
  };
  const tableMutations = new MutationObserver((records) => {
    for (const record of records) {
      for (const node of record.addedNodes) {
        if (!(node instanceof Element)) continue;
        watchTables(node);
        watchVersions(node);
      }
    }
  });

  const onContextMenu = (e) => {
    const send = paneOf(e.target);
    if (!send || !(e.target instanceof Element)) return;
    const t = e.target;
    if (t.closest("input, textarea, [contenteditable=true], .chat-composer, .more-panel, .topic-sidebar, .chat-head, .file-card, .multi-bar")) return;
    e.preventDefault();
    if (!t.closest(".chat-list")) return;
    const hit = t.closest(".bubble, .msg-reactions, .msg-annotation");
    const sender = t.closest(".msg-avatar, .msg-name");
    const sel = document.getSelection();
    const row = t.closest("[data-msg]");
    let text = null;
    let html = null;
    if (sel && !sel.isCollapsed && row && sel.containsNode(row, true)) {
      const payload = selectionPayload(sel);
      text = payload.text;
      html = payload.html;
    }
    const image = t.closest(".img-preview");
    send({
      t: "menu",
      msg: (hit || sender) && row ? Number(row.dataset.msg) : null,
      sender: Boolean(sender),
      x: e.clientX,
      y: e.clientY,
      image: image ? Number(image.dataset.index) : null,
      text,
      html,
    });
  };

  const onCopy = (e) => {
    const sel = document.getSelection();
    if (!sel || sel.isCollapsed) return;
    const send = paneOf(sel.anchorNode);
    if (!send) return;
    const anchor = sel.anchorNode instanceof Element ? sel.anchorNode : sel.anchorNode.parentElement;
    if (anchor.closest("input, textarea, [contenteditable=true]") || !anchor.closest(".chat-list")) return;
    const { text, html } = selectionPayload(sel);
    e.clipboardData.setData("text/plain", text);
    e.clipboardData.setData("text/html", html);
    e.preventDefault();
    send({ t: "copy", text, html });
  };

  const onMouseUp = (e) => {
    const send = paneOf(e.target);
    if (!send || !(e.target instanceof Element)) return;
    const msg = msgOf(e.target);
    if (msg === null) return;
    send({
      t: "gesture",
      msg,
      button: e.button,
      clicks: e.detail,
      ctrl: e.ctrlKey,
      alt: e.altKey,
      shift: e.shiftKey,
      meta: e.metaKey,
    });
  };

  // The body is user-select: none, so pressing on a non-selectable spot (blank area, avatar, bubble
  // padding) does not collapse the existing selection by itself; clear it here
  const clearPicked = () => {
    for (const el of document.querySelectorAll(".bubble.picked")) el.classList.remove("picked");
  };
  const onMouseDown = (e) => {
    if (e.button !== 0 || !paneOf(e.target) || !(e.target instanceof Element)) return;
    const t = e.target;
    const list = t.closest(".chat-list");
    if (!list) return;
    // Single-select: the bubble under the pointer turns "picked", any other press clears it.
    // Multi-select mode has its own checked state, so it is left alone.
    clearPicked();
    const bubble = t.closest(".bubble");
    if (bubble && !bubble.classList.contains("bare") && !list.querySelector(".msg-check") && !t.closest("input, textarea, button, a, .img-preview, .reaction-chip")) {
      bubble.classList.add("picked");
    }
    if (t.closest(".selectable, input, textarea, [contenteditable=true]")) return;
    const sel = document.getSelection();
    if (sel && !sel.isCollapsed) sel.removeAllRanges();
  };

  // Left/right arrows switch the edit history of the picked bubble (the buttons under it)
  const onArrowKey = (e) => {
    if (e.defaultPrevented || e.ctrlKey || e.altKey || e.shiftKey || e.metaKey || e.isComposing) return false;
    if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return false;
    if (e.target instanceof Element && e.target.closest("input, textarea, [contenteditable=true]")) return false;
    if (document.querySelector(".overlay-backdrop, .dialog-layer:not(.closing), .menu, .overlay-card")) return false;
    const bubble = document.querySelector(".bubble.picked");
    if (!bubble || !paneOf(bubble)) return false;
    const button = bubble.querySelector(e.key === "ArrowLeft" ? ".ver-prev" : ".ver-next");
    // At the first/last version the button is absent; the key is still consumed by the bubble
    if (bubble.querySelector(".msg-versions")) {
      e.preventDefault();
      if (button) button.click();
      return true;
    }
    return false;
  };

  // The version switch ("< 2/2 >") goes on the last line of the text when that line leaves room for
  // it, otherwise it keeps its own line below
  const layoutVersions = (bar) => {
    const bubble = bar.closest(".bubble");
    if (!bubble) return;
    bar.classList.remove("inline");
    bar.style.marginTop = "";
    // Only when the switch is the last thing in the bubble and the text right above it is plain
    if (bar.nextElementSibling) return;
    const walker = document.createTreeWalker(bubble, NodeFilter.SHOW_TEXT);
    let last = null;
    for (let node = walker.nextNode(); node; node = walker.nextNode()) {
      if (node.textContent.trim() && !bar.contains(node)) last = node;
    }
    if (!last || last.parentElement.closest(".code-block, table, blockquote, .katex, .img-preview, .spoiler-head")) return;
    const range = document.createRange();
    range.selectNodeContents(last);
    const rects = range.getClientRects();
    if (!rects.length) return;
    const line = rects[rects.length - 1];
    const box = { right: bubble.getBoundingClientRect().right - parseFloat(getComputedStyle(bubble).paddingRight) };
    const need = bar.getBoundingClientRect().width + 8;
    const trailing = last.textContent.length - last.textContent.trimEnd().length;
    if (box.right - line.right >= need && trailing < 2) {
      bar.classList.add("inline");
      bar.style.marginTop = `${-bar.getBoundingClientRect().height}px`;
    }
  };
  const versionBars = new WeakSet();
  const watchVersions = (root) => {
    const bars = root.matches?.(".msg-versions") ? [root] : [];
    if (root.querySelectorAll) bars.push(...root.querySelectorAll(".msg-versions"));
    for (const bar of bars) {
      if (versionBars.has(bar)) {
        layoutVersions(bar);
        continue;
      }
      versionBars.add(bar);
      const bubble = bar.closest(".bubble");
      if (bubble) {
        const again = () => layoutVersions(bar);
        new ResizeObserver(again).observe(bubble);
        // The content is replaced when another version is shown
        new MutationObserver(again).observe(bubble, { childList: true, subtree: true, characterData: true });
      }
      layoutVersions(bar);
    }
  };

  const onKeyDown = (e) => {
    if (onArrowKey(e)) return;
    if (e.key !== "Escape" || e.defaultPrevented) return;
    clearPicked();
    // When a popover is open, this Esc only closes the popover (onEscape in ui.js)
    if (document.querySelector(".overlay-backdrop, .dialog-layer:not(.closing)")) return;
    for (const send of panes.values()) send({ t: "esc" });
  };

  window.zulipChat = {
    attach(id, send) {
      if (panes.size === 0) {
        document.addEventListener("click", onClick, true);
        document.addEventListener("dblclick", onDblClick, true);
        document.addEventListener("error", onImageError, true);
        document.addEventListener("contextmenu", onContextMenu);
        document.addEventListener("copy", onCopy);
        document.addEventListener("mouseup", onMouseUp);
        document.addEventListener("mousedown", onMouseDown);
        document.addEventListener("keydown", onKeyDown);
        tableMutations.observe(document.body, { childList: true, subtree: true });
        watchTables(document.body);
        watchVersions(document.body);
      }
      panes.set(id, send);
      return new Promise((resolve) => panes.set(id, Object.assign(send, { close: resolve })));
    },
    detach(id) {
      const send = panes.get(id);
      panes.delete(id);
      if (send) send.close();
      if (panes.size === 0) {
        document.removeEventListener("click", onClick, true);
        document.removeEventListener("dblclick", onDblClick, true);
        document.removeEventListener("error", onImageError, true);
        document.removeEventListener("contextmenu", onContextMenu);
        document.removeEventListener("copy", onCopy);
        document.removeEventListener("mouseup", onMouseUp);
        document.removeEventListener("mousedown", onMouseDown);
        document.removeEventListener("keydown", onKeyDown);
        tableMutations.disconnect();
        tableResize.disconnect();
      }
    },
    setImage(scope, url, local) {
      for (const img of document.querySelectorAll(`${scope} img[data-remote]`)) {
        if (img.dataset.remote === url) img.src = local;
      }
    },
    failImage(scope, url) {
      for (const img of document.querySelectorAll(`${scope} img[data-remote]`)) {
        if (img.dataset.remote === url && img.parentElement) img.parentElement.classList.add("failed");
      }
    },
    // A copy of the bubble shrinks and fades as it flies to the main menu button; a standalone chat
    // window without that button flies to the bottom-left corner
    flyToMenu(msg, ms) {
      const bubble = document.querySelector(`.chat-list [data-msg="${msg}"] .bubble`);
      if (!bubble) return;
      const from = bubble.getBoundingClientRect();
      const button = document.querySelector(".main-menu-button");
      const to = button ? button.getBoundingClientRect() : null;
      const x = to ? to.left + to.width / 2 : 24;
      const y = to ? to.top + to.height / 2 : innerHeight - 24;
      const copy = bubble.cloneNode(true);
      copy.classList.add("star-fly");
      copy.style.cssText = `left:${from.left}px;top:${from.top}px;width:${from.width}px;height:${from.height}px;--fly-ms:${ms}ms`;
      document.body.appendChild(copy);
      // Paint one frame at the original position first, so the transition takes effect
      copy.getBoundingClientRect();
      copy.style.left = `${x - from.width / 2}px`;
      copy.style.top = `${y - from.height / 2}px`;
      copy.style.transform = "scale(0.1)";
      copy.style.opacity = "0.2";
      setTimeout(() => copy.remove(), ms);
    },
  };
})();
