// 消息内容是服务器 HTML，里面的点击、右键、复制在这里委托后交给 Rust。
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

  // 选中内容：跨多条消息时每条前面加“名字: 时间”，消息之间空一行
  const selectionPayload = (sel) => {
    const range = sel.getRangeAt(0);
    const start = range.startContainer;
    const root = (start instanceof Element ? start : start.parentElement).closest(".chat-list");
    const rows = root
      ? [...root.querySelectorAll("[data-msg]")].filter((r) => range.intersectsNode(r))
      : [];
    if (rows.length < 2) {
      return { text: sel.toString(), html: serialize(range.cloneContents()) };
    }
    const texts = [];
    const htmls = [];
    for (const row of rows) {
      const part = range.cloneRange();
      if (row.contains(range.startContainer)) part.setEndAfter(row);
      else if (row.contains(range.endContainer)) part.setStartBefore(row);
      else part.selectNode(row);
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

  // 捕获阶段拦截：链接不能进入框架默认的打开方式
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
    // 频道公告不属于任何消息，事件带频道 id
    const notice = t.closest("[data-notice]");
    const noticeStream = notice ? Number(notice.dataset.notice) : null;
    // 多选时点击只用来勾选消息。这里不能拦截事件（消息行要收到点击），链接由 CSS 让点击穿过
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
    // 图片双击打开（onDblClick），单击不做任何事
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
    // 引用中的链接、提及先处理；拖动选择了文字时不跳转
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

  // 图片下载了却显示不出来（文件损坏、格式不支持）：与下载失败一样显示占位框，不露出裂图图标
  const onImageError = (e) => {
    const img = e.target;
    if (!(img instanceof HTMLImageElement) || !img.getAttribute("src")) return;
    const box = img.closest(".img-preview");
    if (box) box.classList.add("failed");
  };

  // 表格列宽：表格占满可用宽度，按各列最长内容的比例分配（放不下的内容自动换行），
  // 每列不小于 TABLE_MIN_COLUMN；每列都到了最小宽度还放不下时横向滚动
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
    // 不换行时每列的宽度就是这一列最长内容的宽度
    table.classList.add("measuring");
    const natural = [...first.cells].map((c) => c.getBoundingClientRect().width);
    // 表格撑满时外层（气泡、引用）都到了最大宽度，这时表格区的宽度就是可用宽度
    table.style.width = "100000px";
    const available = host.getBoundingClientRect().width;
    table.classList.remove("measuring");
    // 合并边框时表格比各列之和多出半条外边框，留 1px 免得出现横向滚动条
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
  // 消息行、输入框中的引用宽度变化（窗口大小、侧栏）时重新计算；只是高度变化不用
  const tableWidths = new WeakMap();
  const tableResize = new ResizeObserver((entries) => {
    for (const entry of entries) {
      // 移出页面（虚拟列表回收）时也会回调：不再观察
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
      // 第一次观察时也会回调：先记下宽度，不重复计算
      if (!tableWidths.has(box)) tableWidths.set(box, Math.round(box.getBoundingClientRect().width));
      tableResize.observe(box);
    }
  };
  const tableMutations = new MutationObserver((records) => {
    for (const record of records) for (const node of record.addedNodes) if (node instanceof Element) watchTables(node);
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

  const onKeyDown = (e) => {
    if (e.key !== "Escape" || e.defaultPrevented) return;
    // 有弹层打开时，这次 Esc 只关闭弹层（ui.js 的 onEscape）
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
        document.addEventListener("keydown", onKeyDown);
        tableMutations.observe(document.body, { childList: true, subtree: true });
        watchTables(document.body);
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
    // 气泡的副本缩小、变淡，飞向主菜单按钮；独立聊天窗口没有按钮时飞向左下角
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
      // 先按原位置绘制一帧，过渡才会生效
      copy.getBoundingClientRect();
      copy.style.left = `${x - from.width / 2}px`;
      copy.style.top = `${y - from.height / 2}px`;
      copy.style.transform = "scale(0.1)";
      copy.style.opacity = "0.2";
      setTimeout(() => copy.remove(), ms);
    },
  };
})();
