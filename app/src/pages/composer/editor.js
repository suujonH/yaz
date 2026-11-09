// 输入框的 DOM 集成：选区、IME、按键转发、补丁、测量。内容模型和渲染都在 Rust，这里只认结构：
// 编辑区的子元素是块（.zl 一行；带 data-src 的是不可编辑的整块），每个块的文字拼起来就是 Markdown 原文。
// 偏移都是 UTF-16。
(() => {
  if (window.zulipComposer) return;
  const editors = new Map();
  const BLOCK = /^(DIV|P|LI|UL|OL|TABLE|TR|TD|BLOCKQUOTE|PRE|H[1-6])$/;
  const CELL_CHROME = 21; // 内边距 8 + 8，边框 1，光标余量 4
  const FILE_CHUNK = 1 << 20;
  const HEX = Array.from({ length: 256 }, (_, i) => i.toString(16).padStart(2, "0"));

  const isBlock = (n) => n.nodeType === 1 && BLOCK.test(n.tagName);
  const isPhantom = (n) => n.classList.contains("zphantom") && !n.textContent;
  const isAtomic = (n) => n.dataset.src !== undefined;
  // 行首的引用、列表符号（与 zulip-core 的 markdown::markup 相同）：有符号的行换行、退格交给 Rust 续写 / 删除
  const MARKUP = /^(?: {0,3}> ?)*(?: *(?:[*+-]|\d{1,9}\.) +(?:\[[ xX]\] +)?)?/;
  const markupLength = (line) => MARKUP.exec(serialize(line))[0].length;
  // 开头围栏（与 zulip-core 的 code_language 相同）：行末回车交给 Rust 补成代码块
  const OPEN_FENCE = /^(?: {0,3}> ?)*(?:`{3,}|~{3,})[^\s`~]*$/;

  // 容器的原文。points 里的 {node, offset} 会得到 at（原文偏移）
  function serialize(root, points = []) {
    let out = "", started = false, open = false, lineStart = 0;
    const hit = (n, o) => {
      for (const p of points) if (p.at === undefined && p.node === n && p.offset === o) p.at = out.length;
    };
    const begin = () => {
      if (started) out += "\n";
      started = open = true;
      lineStart = out.length;
    };
    const visit = (parent) => {
      const kids = parent.childNodes;
      for (let i = 0; i < kids.length; i++) {
        hit(parent, i);
        const n = kids[i];
        if (n.nodeType === 3) {
          if (!open) begin();
          for (const p of points) if (p.at === undefined && p.node === n) p.at = out.length + Math.min(p.offset, n.length);
          out += n.data;
        } else if (n.nodeType !== 1) {
          continue;
        } else if (isAtomic(n)) {
          const block = isBlock(n);
          if (block || !open) begin();
          for (const p of points) if (p.at === undefined && n.contains(p.node)) p.at = out.length;
          out += n.dataset.src;
          if (block) open = false;
        } else if (n.tagName === "BR") {
          if (!open) begin();
          if (i < kids.length - 1) open = false;
        } else if (isPhantom(n)) {
          for (const p of points) if (p.at === undefined && n.contains(p.node)) p.at = out.length;
        } else if (isBlock(n)) {
          if (!(open && out.length === lineStart)) begin();
          visit(n);
          open = false;
        } else {
          visit(n);
        }
      }
      hit(parent, kids.length);
    };
    visit(root);
    return out;
  }

  const isHidden = (text) => {
    const mark = text.parentElement && text.parentElement.closest(".zm");
    return !!mark && getComputedStyle(mark).display === "none";
  };

  // 行内相对偏移 → DOM 位置；隐藏的符号上不能放光标，落到后面可见的位置
  function inLine(el, rel) {
    let fallback = null;
    const go = (parent) => {
      const kids = parent.childNodes;
      for (let i = 0; i < kids.length; i++) {
        const n = kids[i];
        if (n.nodeType === 3) {
          if (isHidden(n)) {
            rel = rel <= n.length ? 0 : rel - n.length;
          } else if (rel <= n.length) {
            return [n, rel];
          } else {
            rel -= n.length;
            fallback = [n, n.length];
          }
        } else if (n.nodeType === 1) {
          if (isAtomic(n)) {
            if (rel === 0) return [parent, i];
            rel = Math.max(0, rel - n.dataset.src.length);
            fallback = [parent, i + 1];
          } else if (n.tagName === "BR") {
            if (rel === 0) return [parent, i];
          } else {
            const found = go(n);
            if (found) return found;
          }
        }
      }
      return null;
    };
    return go(el) || fallback || [el, 0];
  }

  // 原文偏移所在的块和相对偏移
  function blockAt(root, offset) {
    const kids = root.children;
    let pos = 0;
    for (let i = 0; i < kids.length; i++) {
      const el = kids[i];
      if (isPhantom(el)) {
        if (offset <= pos) return { el, rel: 0 };
        continue;
      }
      const len = isAtomic(el) ? el.dataset.src.length : serialize(el).length;
      if (isAtomic(el)) {
        if (offset < pos + len || i === kids.length - 1) return { index: i };
      } else if (offset <= pos + len || i === kids.length - 1) {
        return { el, rel: Math.max(0, offset - pos) };
      }
      pos += len + 1;
    }
    return { index: kids.length };
  }

  function locate(root, offset) {
    const at = blockAt(root, offset);
    return at.el ? inLine(at.el, at.rel) : [root, at.index];
  }

  const lineCount = (el) => (isPhantom(el) ? 0 : isAtomic(el) ? el.dataset.src.split("\n").length : 1);

  class Editor {
    constructor(el, send) {
      this.el = el;
      this.send = send;
      this.rev = 0;
      this.seq = 0;
      this.composing = false;
      this.held = null;
      this.pressed = false;
      this.opts = {};
      this.fileId = 0;
      this.measured = new Map();
      this.lastSel = "";
      this.lastRect = "";
      this.handlers = [];
      const on = (target, type, fn, opts) => {
        target.addEventListener(type, fn, opts);
        this.handlers.push([target, type, fn, opts]);
      };
      on(el, "beforeinput", (e) => this.onBeforeInput(e));
      on(el, "input", (e) => {
        if (!e.isComposing && !this.composing) this.syncFrom(e.target);
      });
      on(el, "compositionstart", () => {
        this.composing = true;
        this.send({ t: "comp", on: true });
      });
      on(el, "compositionend", (e) => {
        this.composing = false;
        this.syncFrom(e.target);
        this.send({ t: "comp", on: false });
        if (this.held) {
          const m = this.held;
          this.held = null;
          this.apply(m);
        }
      });
      on(el, "keydown", (e) => this.onKey(e));
      on(el, "paste", (e) => this.onPaste(e));
      on(el, "copy", (e) => {
        e.preventDefault();
        this.send({ t: "cmd", cmd: "copy" });
      });
      on(el, "cut", (e) => {
        e.preventDefault();
        this.send({ t: "cmd", cmd: "cut" });
      });
      on(el, "dragstart", (e) => e.preventDefault());
      on(el, "mousedown", (e) => {
        this.pressed = true;
        if (e.target.closest("[data-act]")) e.preventDefault();
      });
      on(el, "click", (e) => this.onClick(e));
      on(document, "mouseup", () => {
        if (this.pressed) {
          this.pressed = false;
          this.report();
        }
      });
      on(document, "selectionchange", () => {
        if (!this.composing && !this.pressed) this.report();
      });
      on(el, "focusin", () => {
        this.send({ t: "focus", on: true });
        this.report();
      });
      on(el, "focusout", (e) => {
        if (e.relatedTarget && el.contains(e.relatedTarget)) return;
        this.clearReveal();
        this.send({ t: "focus", on: false });
      });
      on(el, "scroll", () => this.reportRect(), true);
      this.resize = new ResizeObserver(() => this.measureTables());
      this.resize.observe(el);
      this.send({ t: "ready" });
    }

    detach() {
      this.resize.disconnect();
      for (const [target, type, fn, opts] of this.handlers) target.removeEventListener(type, fn, opts);
    }

    // ───────── 补丁 ─────────

    apply(m) {
      if (m.opts) Object.assign(this.opts, m.opts);
      if (m.widths) this.setWidths(m.widths);
      if (m.focus && !m.blocks) {
        this.placeSel(this.lastPlaced);
      }
      if (!m.blocks) return;
      if (m.force) {
        if (this.composing) {
          this.held = m;
          return;
        }
        this.rev = m.rev;
      } else if (m.seq !== this.seq || this.composing) {
        return;
      }
      const anchor = m.keepScroll ? this.scrollAnchor() : null;
      const before = m.sel ? null : this.readSel();
      const changed = this.patch(this.el, m.blocks);
      if (anchor) this.restoreScroll(anchor);
      if (m.sel) {
        this.placeSel(m.sel, m.focus);
      } else if (changed && before) {
        this.placeSel(before, false);
      }
      this.measureTables();
      this.report();
    }

    // 只替换变化的块。比较时也看当前的文字：原生输入改过的块要换掉
    patch(root, blocks) {
      const clean = (node) =>
        node.classList.contains("zt")
          ? [...node.querySelectorAll("td.zc")].every((td) => serialize(td) === td._lines)
          : isAtomic(node) || serialize(node) === node._src;
      const same = (node, b) => node._key === b.key && clean(node);
      const kids = [...root.children];
      let head = 0;
      while (head < kids.length && head < blocks.length && same(kids[head], blocks[head])) head++;
      let tail = 0;
      while (
        tail < kids.length - head &&
        tail < blocks.length - head &&
        same(kids[kids.length - 1 - tail], blocks[blocks.length - 1 - tail])
      )
        tail++;
      const removeEnd = kids.length - tail;
      const insertEnd = blocks.length - tail;
      if (head === removeEnd && head === insertEnd && root.childNodes.length === kids.length) return false;
      // 原生输入可能在块之外留下文字节点
      for (const n of [...root.childNodes]) if (n.nodeType !== 1) n.remove();
      const before = kids[removeEnd] || null;
      for (let i = head; i < insertEnd; i++) {
        const b = blocks[i];
        const old = kids[i] && i < removeEnd ? kids[i] : null;
        if (b.table && old && old.classList.contains("zt") && this.patchTable(old, b)) continue;
        const node = this.build(b);
        if (old) old.replaceWith(node);
        else root.insertBefore(node, before);
      }
      for (let i = insertEnd; i < removeEnd; i++) kids[i].remove();
      return true;
    }

    build(b) {
      let node;
      if (b.table) {
        node = document.createElement("div");
        node.className = "zt";
        node.contentEditable = "false";
        const scroll = document.createElement("div");
        scroll.className = "zt-scroll scroll";
        const table = document.createElement("table");
        table.append(document.createElement("colgroup"), document.createElement("tbody"));
        scroll.append(table);
        node.append(scroll);
        this.patchTable(node, b, true);
      } else {
        const t = document.createElement("template");
        t.innerHTML = b.html;
        node = t.content.firstElementChild;
      }
      node._key = b.key;
      node._src = isAtomic(node) ? null : serialize(node);
      return node;
    }

    patchTable(node, b, fresh) {
      const t = b.table;
      const table = node.querySelector("table");
      const body = table.tBodies[0];
      const dims = (rows) => rows.length + ":" + (rows[0] ? rows[0].length : 0);
      const current = [...body.rows].map((r) => [...r.cells]);
      if (!fresh && dims(current) !== dims(t.rows)) return false;
      node.dataset.src = t.src;
      node.dataset.t = t.index;
      node._key = b.key;
      const cols = table.querySelector("colgroup");
      if (cols.children.length !== t.rows[0].length) {
        cols.replaceChildren(...t.rows[0].map(() => document.createElement("col")));
        this.measured.delete(t.index);
      }
      t.rows.forEach((row, r) => {
        const tr = fresh ? body.insertRow() : body.rows[r];
        row.forEach((lines, c) => {
          let td = fresh ? null : current[r][c];
          if (!td) {
            td = tr.insertCell();
            td.className = r === 0 ? "zc zc-head" : "zc";
            td.contentEditable = "true";
            td.spellcheck = false;
            td.dataset.r = r;
            td.dataset.c = c;
          }
          td.style.textAlign = t.aligns[c] || "left";
          this.patch(td, lines);
          td._lines = serialize(td);
        });
      });
      return true;
    }

    scrollAnchor() {
      const top = this.el.getBoundingClientRect().top;
      let line = 0;
      for (const kid of this.el.children) {
        const rect = kid.getBoundingClientRect();
        if (rect.bottom > top) return { line, off: rect.top - top };
        line += lineCount(kid);
      }
      return null;
    }

    restoreScroll(anchor) {
      let line = 0;
      const top = this.el.getBoundingClientRect().top;
      for (const kid of this.el.children) {
        const count = lineCount(kid);
        if (line + count > anchor.line || (count && line === anchor.line)) {
          this.el.scrollTop += kid.getBoundingClientRect().top - top - anchor.off;
          return;
        }
        line += count;
      }
    }

    // ───────── 选区 ─────────

    cellOf(node) {
      const el = node && (node.nodeType === 1 ? node : node.parentElement);
      const td = el && el.closest("td.zc");
      return td && this.el.contains(td) ? td : null;
    }

    readSel() {
      const sel = getSelection();
      if (!sel.rangeCount || !this.el.contains(sel.anchorNode)) return null;
      const td = this.cellOf(sel.anchorNode);
      const host = td && td.contains(sel.focusNode) ? td : this.el;
      const a = { node: sel.anchorNode, offset: sel.anchorOffset };
      const f = { node: sel.focusNode, offset: sel.focusOffset };
      const hidden = [...host.querySelectorAll(".zm")]
        .map((m) => m.firstChild)
        .filter((t) => t && isHidden(t))
        .map((t) => ({ node: t, offset: 0, len: t.length }));
      const total = serialize(host, [a, f, ...hidden]).length;
      const x = a.at || 0;
      const y = f.at || 0;
      let s = Math.min(x, y);
      let e = Math.max(x, y);
      // 选中了全部显示的内容（全选）：首尾隐藏的符号也算选中
      if (s !== e) {
        let from = s;
        let to = e;
        for (const h of hidden) if (h.at + h.len === from) from = h.at;
        for (const h of [...hidden].reverse()) if (h.at === to) to += h.len;
        if (from === 0 && to === total) {
          s = from;
          e = to;
        }
      }
      return {
        s,
        e,
        cell: host === td ? [+td.closest(".zt").dataset.t, +td.dataset.r, +td.dataset.c] : null,
      };
    }

    hostOf(sel) {
      if (!sel.cell) return this.el;
      const table = this.el.querySelector(`:scope > .zt[data-t="${sel.cell[0]}"]`);
      return (table && table.querySelector(`td[data-r="${sel.cell[1]}"][data-c="${sel.cell[2]}"]`)) || this.el;
    }

    placeSel(sel, focus = true) {
      if (!sel) return;
      this.lastPlaced = sel;
      const host = this.hostOf(sel);
      if (focus) host.focus({ preventScroll: true });
      if (sel.s === sel.e) this.setReveal(host, sel.s);
      const [an, ao] = locate(host, sel.s);
      const [fn, fo] = locate(host, sel.e);
      const range = getSelection();
      range.setBaseAndExtent(an, ao, fn, fo);
      this.revealCaret();
    }

    clearReveal() {
      for (const open of this.el.querySelectorAll(".zr.open")) open.classList.remove("open");
    }

    setReveal(host, offset) {
      this.clearReveal();
      const at = blockAt(host, offset);
      if (!at.el) return;
      this.revealIn(at.el, at.rel);
    }

    revealIn(line, rel) {
      const targets = [...line.querySelectorAll(".zr")];
      if (line.matches(".zr")) targets.push(line);
      for (const el of targets) el.classList.toggle("open", +el.dataset.s <= rel && rel <= +el.dataset.e);
    }

    // 光标所在行和相对偏移
    caretLine() {
      const sel = getSelection();
      if (!sel.rangeCount || !sel.isCollapsed || !this.el.contains(sel.anchorNode)) return null;
      const node = sel.anchorNode.nodeType === 1 ? sel.anchorNode : sel.anchorNode.parentElement;
      const line = node.closest(".zl");
      if (!line) return null;
      const p = { node: sel.anchorNode, offset: sel.anchorOffset };
      serialize(line, [p]);
      return { line, rel: p.at || 0, td: this.cellOf(line) };
    }

    caretRect() {
      const sel = getSelection();
      if (!sel.rangeCount) return null;
      const range = sel.getRangeAt(0).cloneRange();
      range.collapse(false);
      let rect = range.getClientRects()[0];
      if (!rect) {
        const node = range.startContainer.nodeType === 1 ? range.startContainer : range.startContainer.parentElement;
        rect = node.getBoundingClientRect();
      }
      return rect;
    }

    revealCaret() {
      const rect = this.caretRect();
      if (!rect) return;
      const view = this.el.getBoundingClientRect();
      if (rect.bottom > view.bottom) this.el.scrollTop += rect.bottom - view.bottom + 4;
      else if (rect.top < view.top) this.el.scrollTop -= view.top - rect.top + 4;
    }

    report() {
      const sel = this.readSel();
      if (!sel) return;
      if (!this.pressed) {
        const caret = this.caretLine();
        if (caret) {
          this.clearReveal();
          this.revealIn(caret.line, caret.rel);
        } else if (!getSelection().isCollapsed) {
          this.clearReveal();
        }
      }
      const rect = this.caretRect();
      const msg = { t: "sel", rev: this.rev, ...sel, x: rect ? rect.left : 0, y: rect ? rect.top : 0 };
      const key = JSON.stringify(msg);
      if (key !== this.lastSel) {
        this.lastSel = key;
        this.lastPlaced = sel;
        this.send(msg);
      }
      this.reportRect();
    }

    reportRect() {
      const sel = getSelection();
      const td = sel.rangeCount ? this.cellOf(sel.anchorNode) : null;
      let rect = null;
      if (td) {
        const r = td.closest(".zt").getBoundingClientRect();
        const view = this.el.getBoundingClientRect();
        rect = { top: Math.max(0, r.top - view.top), left: r.left - view.left, width: r.width };
      }
      const key = JSON.stringify(rect);
      if (key !== this.lastRect) {
        this.lastRect = key;
        this.send({ t: "tablerect", rect });
      }
    }

    // ───────── 输入 ─────────

    syncFrom(target) {
      this.seq++;
      const td = this.cellOf(target) || this.cellOf(getSelection().anchorNode);
      const sel = this.readSel() || { s: 0, e: 0, cell: null };
      if (td) {
        const table = td.closest(".zt");
        this.send({
          t: "cell",
          seq: this.seq,
          rev: this.rev,
          table: +table.dataset.t,
          row: +td.dataset.r,
          col: +td.dataset.c,
          value: serialize(td),
          s: sel.s,
          e: sel.e,
        });
      } else {
        this.send({ t: "sync", seq: this.seq, rev: this.rev, text: serialize(this.el), s: sel.s, e: sel.e });
      }
    }

    onBeforeInput(e) {
      // 选区的内容由模型替换：原生删除留不下隐藏的符号
      if (e.inputType === "insertText" && !e.isComposing && !getSelection().isCollapsed) {
        e.preventDefault();
        this.send({ t: "insert", text: e.data || "" });
        return;
      }
      switch (e.inputType) {
        case "historyUndo":
          e.preventDefault();
          this.send({ t: "cmd", cmd: "undo" });
          break;
        case "historyRedo":
          e.preventDefault();
          this.send({ t: "cmd", cmd: "redo" });
          break;
        case "insertFromPaste":
        case "insertFromDrop":
        case "deleteByDrag":
        case "insertFromYank":
          e.preventDefault();
          break;
        case "insertParagraph":
        case "insertLineBreak": {
          const caret = this.caretLine();
          if (caret && caret.line.classList.contains("zphantom")) caret.line.classList.remove("zphantom");
          break;
        }
        default:
          if (e.inputType.startsWith("format")) e.preventDefault();
      }
    }

    onKey(e) {
      if (e.isComposing || e.keyCode === 229) return;
      const key = e.key;
      const ctrl = e.ctrlKey || e.metaKey;
      const shift = e.shiftKey;
      const alt = e.altKey;
      const plain = !ctrl && !shift && !alt;
      const cmd = (c) => {
        e.preventDefault();
        this.send({ t: "cmd", cmd: c });
      };
      if (this.opts.popup) {
        if (plain && key === "ArrowUp") return cmd("popup-up");
        if (plain && key === "ArrowDown") return cmd("popup-down");
        if (plain && (key === "Enter" || key === "Tab")) return cmd("popup-accept");
        if (plain && key === "Escape") return cmd("popup-close");
      }
      if (plain && (key === "Backspace" || key === "Delete") && !getSelection().isCollapsed) return cmd("delete-selection");
      const caret = this.caretLine();
      const td = caret ? caret.td : this.cellOf(getSelection().anchorNode);
      if (td && this.onCellKey(e, td, cmd)) return;
      if (ctrl && !alt) {
        const k = key.toLowerCase();
        if (k === "z") return cmd(shift ? "redo" : "undo");
        if (k === "y" && !shift) return cmd("redo");
        if (!this.opts.source) {
          if (k === "b" && !shift) return cmd("bold");
          if (k === "i" && !shift) return cmd("italic");
          if (k === "l" && shift) return cmd("link");
          if (k === "c" && shift) return cmd("code");
          if (!shift && !td && /^[0-6]$/.test(key)) {
            e.preventDefault();
            this.send({ t: "heading", level: +key });
            return;
          }
        }
      }
      if (plain && key === "Escape" && this.opts.editing) return cmd("cancel-edit");
      if (key === "Enter" && !alt) {
        const send = this.opts.enterSends ? !ctrl && !shift : ctrl && !shift;
        if (!td && caret && getSelection().isCollapsed) {
          const text = serialize(caret.line);
          if (caret.rel === text.length && OPEN_FENCE.test(text)) return cmd(send ? "fence-send" : "fence-newline");
        }
        if (send) return cmd("send");
        if (!td && caret && markupLength(caret.line) > 0) return cmd("newline");
        // 换行统一成段落，序列化只认块
        if (ctrl || shift) {
          e.preventDefault();
          document.execCommand("insertParagraph");
        }
        return;
      }
      if (plain && key === "Tab" && !td) {
        e.preventDefault();
        document.execCommand("insertText", false, "  ");
        return;
      }
      if (td || !caret) return;
      // 光标紧跟在列表、引用符号后面：退格删掉最里层的一个符号
      if (plain && key === "Backspace" && caret.rel > 0 && caret.rel === markupLength(caret.line)) return cmd("unprefix");
      if (this.opts.source) return;
      this.onLineKey(e, caret, plain);
    }

    // 紧挨表格、引用的按键：文字不能并进去；行首符号是一个整体
    onLineKey(e, { line, rel }, plain) {
      if (!plain) return;
      const prev = line.previousElementSibling;
      const next = line.nextElementSibling;
      const lineLen = serialize(line).length;
      const start = rel === (+line.dataset.cs || 0);
      const end = rel === lineLen;
      const stop = (msg) => {
        e.preventDefault();
        this.send(msg);
      };
      if (line.classList.contains("zphantom") && (e.key === "Backspace" || e.key === "Delete")) {
        const side = e.key === "Backspace" ? prev : next;
        e.preventDefault();
        if (side && side.classList.contains("zt")) this.enterTable(side, e.key === "Backspace");
        else if (side && side.classList.contains("zq")) this.send({ t: "act", act: "delete", q: +side.dataset.q });
        return;
      }
      switch (e.key) {
        case "Backspace":
          if (start && prev && isAtomic(prev)) {
            if (prev.classList.contains("zq")) return stop({ t: "act", act: "delete", q: +prev.dataset.q });
            if (lineLen === 0) return stop({ t: "cmd", cmd: "merge-up" });
            e.preventDefault();
          } else if (start && rel > 0) {
            stop({ t: "cmd", cmd: "unprefix" });
          }
          break;
        case "Delete":
          if (end && next && isAtomic(next)) {
            if (next.classList.contains("zq")) return stop({ t: "act", act: "delete", q: +next.dataset.q });
            if (lineLen === 0) return stop({ t: "cmd", cmd: "merge-down" });
            e.preventDefault();
          }
          break;
        case "ArrowLeft":
          if (start && prev && prev.classList.contains("zt")) {
            e.preventDefault();
            this.enterTable(prev, true);
          }
          break;
        case "ArrowRight":
          if (end && next && next.classList.contains("zt")) {
            e.preventDefault();
            this.enterTable(next, false);
          }
          break;
        case "ArrowUp":
          if (prev && prev.classList.contains("zt") && this.onEdgeRow(line, true)) {
            e.preventDefault();
            this.enterTable(prev, true);
          }
          break;
        case "ArrowDown":
          if (next && next.classList.contains("zt") && this.onEdgeRow(line, false)) {
            e.preventDefault();
            this.enterTable(next, false);
          }
          break;
      }
    }

    // 光标在元素显示的第一行或最后一行
    onEdgeRow(el, first) {
      const caret = this.caretRect();
      if (!caret) return true;
      const range = document.createRange();
      range.selectNodeContents(el);
      const rects = range.getClientRects();
      if (!rects.length) return true;
      const edge = first ? rects[0] : rects[rects.length - 1];
      return first ? caret.top < edge.bottom - 1 : caret.bottom > edge.top + 1;
    }

    enterTable(table, last) {
      const cells = table.querySelectorAll("td.zc");
      const td = last ? cells[cells.length - 1] : cells[0];
      this.focusCell(td, last);
    }

    focusCell(td, atEnd) {
      const len = serialize(td).length;
      this.placeSel(
        { s: atEnd ? len : 0, e: atEnd ? len : 0, cell: [+td.closest(".zt").dataset.t, +td.dataset.r, +td.dataset.c] },
        true,
      );
    }

    leaveTable(table, above) {
      const sib = above ? table.previousElementSibling : table.nextElementSibling;
      if (!sib || isAtomic(sib)) return;
      sib.focus && this.el.focus({ preventScroll: true });
      const point = above ? inLine(sib, serialize(sib).length) : inLine(sib, 0);
      getSelection().collapse(point[0], point[1]);
      this.revealCaret();
    }

    onCellKey(e, td, cmd) {
      const key = e.key;
      const ctrl = e.ctrlKey || e.metaKey;
      const shift = e.shiftKey;
      const alt = e.altKey;
      const plain = !ctrl && !shift && !alt;
      const table = td.closest(".zt");
      const rows = table.querySelector("table").tBodies[0].rows;
      const r = +td.dataset.r;
      const c = +td.dataset.c;
      const cell = (row, col) => rows[row] && rows[row].cells[col];
      const stop = () => {
        e.preventDefault();
        return true;
      };
      const next = () => {
        const to = cell(r, c + 1) || cell(r + 1, 0);
        if (to) this.focusCell(to, false);
        else this.leaveTable(table, false);
        return stop();
      };
      const previous = () => {
        const above = rows[r - 1];
        const to = cell(r, c - 1) || (above && above.cells[above.cells.length - 1]);
        if (to) this.focusCell(to, true);
        else this.leaveTable(table, true);
        return stop();
      };
      if (ctrl && !shift && !alt && key === "Enter") {
        cmd("table-row-below");
        return true;
      }
      if (alt && !ctrl && !shift && e.code === "KeyA") {
        // 选中整个单元格的内容
        const len = serialize(td).length;
        this.placeSel({ s: 0, e: len, cell: [+table.dataset.t, r, c] }, true);
        return stop();
      }
      if (alt && !ctrl && !shift) {
        const move ={ ArrowUp: "row-up", ArrowDown: "row-down", ArrowLeft: "col-left", ArrowRight: "col-right" }[key];
        if (move) {
          cmd("table-" + move);
          return true;
        }
      }
      if (ctrl && shift && key === "Backspace") {
        cmd("table-del-row");
        return true;
      }
      if (shift && !ctrl && !alt && key === "Enter") {
        document.execCommand("insertParagraph");
        return stop();
      }
      if (shift && !ctrl && !alt && key === "Tab") return previous();
      if (!plain) return false;
      const sel = this.readSel();
      const collapsed = sel && sel.s === sel.e;
      switch (key) {
        case "Enter":
          if (cell(r + 1, c)) this.focusCell(cell(r + 1, c), true);
          else this.leaveTable(table, false);
          return stop();
        case "Tab":
          return next();
        case "Backspace": {
          // 空单元格退回上一格：同一行从右到左，再到上一行的最后一格；
          // 第一格没有上一格：整个表格都空时删除表格，否则不动
          if (!collapsed || serialize(td).length !== 0) return false;
          if (r > 0 || c > 0) return previous();
          if ([...table.querySelectorAll("td.zc")].every((cell) => serialize(cell).length === 0)) cmd("table-delete");
          return stop();
        }
        case "ArrowLeft":
          return collapsed && sel.s === 0 ? previous() : false;
        case "ArrowRight":
          return collapsed && sel.s === serialize(td).length ? next() : false;
        case "ArrowUp":
          if (!this.onEdgeRow(td, true)) return false;
          if (cell(r - 1, c)) this.focusCell(cell(r - 1, c), true);
          else this.leaveTable(table, true);
          return stop();
        case "ArrowDown":
          if (!this.onEdgeRow(td, false)) return false;
          if (cell(r + 1, c)) this.focusCell(cell(r + 1, c), true);
          else this.leaveTable(table, false);
          return stop();
      }
      return false;
    }

    onClick(e) {
      const more = e.target.closest(".zq .table-more");
      if (more) {
        this.send({
          t: "quote-table",
          q: +more.closest(".zq").dataset.q,
          table: +more.closest(".table-host").dataset.table,
          x: e.clientX,
          y: e.clientY,
        });
        return;
      }
      const lang = e.target.closest(".zlang");
      if (lang) {
        const r = lang.getBoundingClientRect();
        this.send({ t: "code-language", line: +lang.dataset.open, x: r.left, y: r.top, w: r.width, h: r.height });
        return;
      }
      const button = e.target.closest("[data-act]");
      if (button) {
        const q = button.closest(".zq");
        this.send({ t: "act", act: button.dataset.act, q: q ? +q.dataset.q : 0 });
        return;
      }
      // 单击提及、表情、图片等整体元素：选中它
      const atom = e.target.closest(".za");
      if (atom && this.el.contains(atom)) {
        const range = document.createRange();
        range.selectNode(atom);
        const sel = getSelection();
        sel.removeAllRanges();
        sel.addRange(range);
      }
    }

    onPaste(e) {
      e.preventDefault();
      const data = e.clipboardData;
      const files = [...data.files];
      if (files.length) {
        for (const file of files) this.sendFile(file);
        return;
      }
      this.send({ t: "paste", text: data.getData("text/plain") });
    }

    async sendFile(file) {
      if (this.opts.maxBytes && file.size > this.opts.maxBytes) {
        this.send({ t: "too-big" });
        return;
      }
      const id = ++this.fileId;
      let offset = 0;
      for (;;) {
        const bytes = new Uint8Array(await file.slice(offset, offset + FILE_CHUNK).arrayBuffer());
        let hex = "";
        for (const b of bytes) hex += HEX[b];
        offset += FILE_CHUNK;
        const last = offset >= file.size;
        this.send({ t: "file", id, name: file.name, hex, last });
        if (last) return;
      }
    }

    // ───────── 表格列宽 ─────────

    measureTables() {
      const tables = this.el.querySelectorAll(":scope > .zt");
      if (!tables.length) return;
      const available = this.el.clientWidth - parseFloat(getComputedStyle(this.el).paddingLeft) * 2 - 1;
      const probe = document.createElement("div");
      probe.className = "zt-probe";
      this.el.append(probe);
      for (const node of tables) {
        const rows = node.querySelector("table").tBodies[0].rows;
        const required = [];
        const minimum = [];
        for (const row of rows) {
          for (const td of row.cells) {
            const c = +td.dataset.c;
            probe.className = td.classList.contains("zc-head") ? "zt-probe zc-head" : "zt-probe";
            probe.innerHTML = td.innerHTML;
            probe.style.whiteSpace = "pre";
            probe.style.width = "max-content";
            const full = probe.getBoundingClientRect().width;
            probe.style.whiteSpace = "pre-wrap";
            probe.style.width = "min-content";
            const least = probe.getBoundingClientRect().width;
            required[c] = Math.max(required[c] || 0, Math.max(full, 16) + CELL_CHROME);
            minimum[c] = Math.max(minimum[c] || 0, Math.max(least, 16) + CELL_CHROME);
          }
        }
        const index = +node.dataset.t;
        const key = JSON.stringify([required, minimum, available]);
        if (this.measured.get(index) !== key) {
          this.measured.set(index, key);
          this.send({ t: "widths", table: index, required, minimum, available });
        }
      }
      probe.remove();
    }

    setWidths({ table, cols }) {
      const node = this.el.querySelector(`:scope > .zt[data-t="${table}"]`);
      if (!node) return;
      const el = node.querySelector("table");
      const list = el.querySelectorAll("col");
      let sum = 0;
      let previous = 0;
      cols.forEach((w, i) => {
        sum += w;
        // 竖线落在整像素上
        const edge = Math.round(sum);
        if (list[i]) list[i].style.width = edge - previous + "px";
        previous = edge;
      });
      el.style.width = previous + "px";
    }
  }

  window.zulipComposer = {
    attach(id, send) {
      const el = document.getElementById(id);
      const editor = new Editor(el, send);
      editors.set(id, editor);
      return editor;
    },
    detach(id) {
      const editor = editors.get(id);
      if (editor) {
        editors.delete(id);
        editor.detach();
      }
    },
  };
})();
