// DOM integration of the composer: selection, IME, key forwarding, patches, measurement. The
// content model and rendering are in Rust; here only the structure matters: children of the editing
// area are blocks (.zl is one line; one with data-src is a non-editable whole block), and the text
// of all blocks joined together is the Markdown source. All offsets are UTF-16.
(() => {
  if (window.zulipComposer) return;
  const editors = new Map();
  const BLOCK = /^(DIV|P|LI|UL|OL|TABLE|TR|TD|BLOCKQUOTE|PRE|H[1-6])$/;
  const CELL_CHROME = 21; // Padding 8 + 8, border 1, cursor allowance 4
  const FILE_CHUNK = 1 << 20;
  const HEX = Array.from({ length: 256 }, (_, i) => i.toString(16).padStart(2, "0"));

  const isBlock = (n) => n.nodeType === 1 && BLOCK.test(n.tagName);
  const isPhantom = (n) => n.classList.contains("zphantom") && !n.textContent;
  const isAtomic = (n) => n.dataset.src !== undefined;
  // A line of a code block: its block ends at the closing line holding the language box
  const inCodeBlock = (line) => {
    let n = line;
    while (n && n.matches(".zl-bc")) n = n.nextElementSibling;
    return n !== line && !!n && !!n.querySelector(".zlang-input");
  };
  // Quote and list markers at the line start (same as markdown::markup in zulip-core): for a line
  // with a marker, line break and Backspace are left to Rust to continue / delete
  // Line-start symbols: quotes, then list symbols (with their indentation; several are nested lists) or indentation alone
  const MARKUP = /^(?: {0,3}> ?)*(?:(?: *(?:[*+-]|\d{1,9}\.) +)+(?:\[[ xX]\] +)?| +)?/;
  const markupLength = (line) => MARKUP.exec(serialize(line))[0].length;
  // Opening fence (same as code_language in zulip-core): Enter at the end of the line is left to
  // Rust to complete into a code block
  const OPEN_FENCE = /^(?: {0,3}> ?)*(?:`{3,}|~{3,})[^\s`~]*$/;

  // Source text of a container. {node, offset} entries in points get an `at` (source offset)
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

  // In-line relative offset -> DOM position; the cursor cannot be placed on hidden markers, so it
  // falls on the next visible position
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

  // The block and relative offset for a source offset
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
    // Indentation spaces (.zi) are one character column (1ch) wide, so a cursor anywhere in them is
    // where the text after it starts: the gap between a space and a "0" in the editor's font, as
    // extra letter spacing (--zsp; word spacing would miss the last space of a line). Measured in
    // the editing area itself (its font), against a 10ch box (the unit the boxes are sized in).
    measureSpaces() {
      const hidden = "position:absolute;visibility:hidden;white-space:pre;letter-spacing:0;";
      const columns = document.createElement("span");
      columns.style.cssText = `${hidden}display:inline-block;width:10ch`;
      const spaces = document.createElement("span");
      spaces.style.cssText = hidden;
      spaces.textContent = " ".repeat(10);
      this.el.append(columns, spaces);
      const column = columns.getBoundingClientRect().width / 10;
      const space = spaces.getBoundingClientRect().width / 10;
      columns.remove();
      spaces.remove();
      if (column > 0) this.el.style.setProperty("--zsp", `${Math.max(0, column - space)}px`);
    }

    constructor(el, send) {
      this.el = el;
      this.send = send;
      this.rev = 0;
      this.seq = 0;
      this.composing = false;
      this.held = null;
      this.pressed = false;
      // Home / End just pressed: the move past hidden markers is finished in report
      this.homeEnd = null;
      // Where the focus last was in the editor: "editor" (the editing area; its selection is
      // lastPlaced) or a code block's language box. Kept while the focus goes nowhere (switching to
      // another program, the webview's focus juggling on return) and given back when the window is
      // focused again; dropped once the user moves on to something else.
      this.lastFocus = null;
      // The window was left for another program (until the next press or key), and the press
      // that brought it back is in progress
      this.away = false;
      this.returnClick = false;
      // The line Up / Down left: entering a span is finished in report
      this.vertical = null;
      this.opts = {};
      this.fileId = 0;
      this.measured = new Map();
      this.lastSel = "";
      this.measureSpaces();
      // The editor's font may load only after the editor shows text (measured with a fallback font
      // until then): measured again whenever a font finishes loading
      if (document.fonts) {
        document.fonts.ready.then(() => this.measureSpaces());
        document.fonts.addEventListener("loadingdone", () => this.measureSpaces());
      }
      this.lastRect = "";
      this.handlers = [];
      const raw = (target, type, fn, opts) => {
        target.addEventListener(type, fn, opts);
        this.handlers.push([target, type, fn, opts]);
      };
      // The language box under a code block is an ordinary text box: its events are not editing
      const langBox = (n) => (n && n.nodeType === 1 && n.matches(".zlang-input") ? n : null);
      const on = (target, type, fn, opts) =>
        raw(target, type, target === el ? (e) => { if (!langBox(e.target)) fn(e); } : fn, opts);
      const langRect = (box) => {
        const r = box.getBoundingClientRect();
        return { x: r.left, y: r.top, w: r.width, h: r.height };
      };
      raw(el, "input", (e) => {
        const box = langBox(e.target);
        if (!box) return;
        // Typing opens the language list; until then Up / Down leave the box
        box.dataset.typed = "1";
        this.send({ t: "lang-query", line: +box.dataset.open, query: box.value, ...langRect(box) });
      });
      raw(el, "keydown", (e) => {
        const box = langBox(e.target);
        if (!box || e.isComposing) return;
        if ((e.key === "ArrowUp" || e.key === "ArrowDown") && !box.dataset.typed) {
          e.preventDefault();
          e.stopPropagation();
          this.leaveLangBox(box, e.key === "ArrowUp");
          return;
        }
        const key = { ArrowDown: "down", ArrowUp: "up", Enter: "enter", Escape: "escape" }[e.key];
        if (!key) return;
        e.preventDefault();
        e.stopPropagation();
        this.send({ t: "lang-key", key });
        if (key === "escape") {
          this.leaveFocus();
          box.blur();
        }
      });
      raw(el, "focusout", (e) => {
        const box = langBox(e.target);
        // The focus going nowhere (another program, the webview's juggling) keeps the box as it is
        if (!box || !e.relatedTarget) return;
        if (el.contains(e.relatedTarget)) this.resetLangBox(box);
        else this.leaveFocus();
      });
      raw(el, "focusin", (e) => {
        const box = langBox(e.target);
        if (!box) return;
        this.lastFocus = box;
        this.markActiveCode(box.closest(".zl"));
      });
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
      // Home / End that did not move the cursor leave nothing to finish
      on(el, "keyup", () => {
        this.homeEnd = null;
        this.vertical = null;
      });
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
      // Ctrl held: links show they can be opened, and Ctrl + click opens them
      const setCtrl = (e) => el.classList.toggle("zctrl", e.ctrlKey || e.metaKey);
      on(document, "keydown", setCtrl);
      on(document, "keyup", setCtrl);
      on(el, "mousemove", setCtrl);
      on(window, "blur", () => el.classList.remove("zctrl"));
      // Back from another program the last focus place takes the focus again when it went
      // nowhere: on the window's focus (the webview may leave it on the page body), and after the
      // click that brought the window back (a click on a blank part, the title bar, takes it off)
      on(window, "blur", () => {
        this.away = true;
      });
      on(window, "focus", () => this.restoreFocus());
      on(
        document,
        "mousedown",
        () => {
          this.returnClick = this.away;
          this.away = false;
        },
        true,
      );
      on(document, "mouseup", () => {
        if (!this.returnClick) return;
        this.returnClick = false;
        // A drag that selected text elsewhere keeps its selection
        const sel = getSelection();
        if (sel.rangeCount && !sel.isCollapsed) return;
        this.restoreFocus();
      });
      on(document, "keydown", () => {
        this.away = false;
      });
      on(el, "mousedown", (e) => {
        const link = (e.ctrlKey || e.metaKey) && e.button === 0 && e.target.closest("[data-href]");
        if (link && el.contains(link)) {
          e.preventDefault();
          this.send({ t: "open-link", href: link.dataset.href });
          return;
        }
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
        this.lastFocus = "editor";
        this.send({ t: "focus", on: true });
        // Focused by a press (e.g. from the language box): the selection is still the old one
        // until the press places it, so it is reported on release instead
        if (!this.pressed) this.report();
      });
      on(el, "focusout", (e) => {
        // Within the editor, or going nowhere (another program, the webview's juggling): the
        // cursor's markers and language box stay as they are
        if (!e.relatedTarget || el.contains(e.relatedTarget)) return;
        this.leaveFocus();
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

    // --------- Patches ---------

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

    // Replace only changed blocks. The comparison also looks at the current text: blocks changed by
    // native input must be replaced
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
      // Native input may leave text nodes outside the blocks
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

    // --------- Selection ---------

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
      // The whole displayed content is selected (select all): hidden markers at the start and end
      // count as selected too
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

    // Show the markers of the span holding the cursor. Showing or hiding markers moves the text, so
    // a selection keeps whatever is shown: selecting never shifts the text it is selecting.
    applyReveal() {
      const caret = this.caretLine();
      if (caret) this.revealIn(caret.line, caret.rel);
    }

    clearReveal() {
      for (const open of this.el.querySelectorAll(".zr.open")) open.classList.remove("open");
    }

    setReveal(host, offset) {
      const at = blockAt(host, offset);
      if (!at.el) {
        this.clearReveal();
        return;
      }
      this.revealIn(at.el, at.rel);
    }

    // Open the markers around the cursor at `rel` of `line`, close the rest. A span opens as soon
    // as the cursor reaches it (its edges included) and closes only once the cursor fully leaves
    // it; a marker line (fence, rule) opens anywhere on it.
    revealIn(line, rel) {
      const targets = [...line.querySelectorAll(".zr")];
      if (line.matches(".zr")) targets.push(line);
      for (const open of this.el.querySelectorAll(".zr.open")) {
        if (!targets.includes(open)) open.classList.remove("open");
      }
      for (const el of targets) el.classList.toggle("open", +el.dataset.s <= rel && rel <= +el.dataset.e);
    }

    // Left / Right on the edge of a closed span steps into it: the span opens and the cursor goes
    // one unit inside (next to its marker) instead of jumping over the hidden markers
    stepIntoSpan(e, line, rel, left) {
      const span = [...line.querySelectorAll(".zr")].find(
        (el) => !el.classList.contains("open") && +(left ? el.dataset.e : el.dataset.s) === rel,
      );
      if (!span) return false;
      e.preventDefault();
      const target = left ? rel - 1 : rel + 1;
      // Open first: the target is next to a marker, which an open span holds on to
      span.classList.add("open");
      this.revealIn(line, target);
      const [node, offset] = inLine(line, target);
      getSelection().collapse(node, offset);
      this.revealCaret();
      return true;
    }

    // Left / Right one unit away from a closed span reaches its outer edge: the span opens and the
    // cursor stops outside its markers (after the closing one, before the opening one). The
    // browser would put it next to the visible text instead, past the hidden markers.
    approachSpan(e, line, rel, left) {
      const target = left ? rel - 1 : rel + 1;
      const span = [...line.querySelectorAll(".zr")].find(
        (el) => !el.classList.contains("open") && +(left ? el.dataset.e : el.dataset.s) === target,
      );
      if (!span) return false;
      e.preventDefault();
      this.revealIn(line, target);
      const [node, offset] = inLine(line, target);
      getSelection().collapse(node, offset);
      this.revealCaret();
      return true;
    }

    // The line holding the cursor and the relative offset
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

    // Home / End stop next to the visible text, after (before) the hidden markers of a span that
    // starts (ends) the row: move on past those markers, so the cursor is at the very start (end)
    // and the span opens around it
    finishHomeEnd() {
      const home = this.homeEnd === "Home";
      this.homeEnd = null;
      const caret = this.caretLine();
      if (!caret) return;
      const spans = [...caret.line.querySelectorAll(".zr")];
      const mark = (span) => {
        const m = home ? span.firstElementChild : span.lastElementChild;
        return m && m.classList.contains("zm") ? m.textContent.length : 0;
      };
      let target = caret.rel;
      for (let moved = true; moved; ) {
        moved = false;
        for (const span of spans) {
          const len = mark(span);
          const edge = home ? +span.dataset.s : +span.dataset.e;
          if (len && (home ? edge + len : edge - len) === target) {
            target = edge;
            moved = true;
          }
        }
      }
      if (target === caret.rel) return;
      this.revealIn(caret.line, target);
      const [node, offset] = inLine(caret.line, target);
      getSelection().collapse(node, offset);
    }

    // Up / Down from another line into a span: the cursor goes to the end of the span's text,
    // before its closing marker (the end of the code / bold / italic text, the end of a link's
    // text before `]`). The innermost span holding the landing place counts.
    finishVertical() {
      const from = this.vertical;
      this.vertical = null;
      const caret = this.caretLine();
      if (!caret || caret.line === from) return;
      const { line, rel } = caret;
      let span = null;
      for (const el of line.querySelectorAll(".zr")) {
        if (el.dataset.ie !== undefined && +el.dataset.s <= rel && rel <= +el.dataset.e) span = el;
      }
      if (!span) return;
      const target = +span.dataset.ie;
      this.revealIn(line, target);
      const [node, offset] = inLine(line, target);
      getSelection().collapse(node, offset);
    }

    // Up / Down in a code block's language box (nothing typed): Up goes to the end of the code's
    // last line, Down to the start of the line after the block
    leaveLangBox(box, up) {
      const close = box.closest(".zl");
      const line = close && (up ? close.previousElementSibling : close.nextElementSibling);
      if (!line || isAtomic(line)) return;
      // The focus moving to the editing area resets the box (see its focusout)
      this.el.focus({ preventScroll: true });
      const [node, offset] = inLine(line, up ? serialize(line).length : 0);
      getSelection().collapse(node, offset);
      this.revealCaret();
    }

    // The language box of a code block shows (over what follows the block) only while the cursor or
    // the selection end is in that block, or the box has the focus. `close` is the closing line to
    // show; without it, the one of the cursor's block (`null`: none).
    markActiveCode(close) {
      if (close === undefined) {
        close = null;
        const sel = getSelection();
        const node = sel.rangeCount && this.el.contains(sel.focusNode) ? sel.focusNode : null;
        const at = node && (node.nodeType === 1 ? node : node.parentElement);
        let line = at && at.closest(".zl");
        if (line && inCodeBlock(line)) {
          while (line.matches(".zl-bc")) line = line.nextElementSibling;
          close = line;
        }
      }
      for (const n of this.el.querySelectorAll(".zl-close.zactive")) if (n !== close) n.classList.remove("zactive");
      if (close) close.classList.add("zactive");
    }

    // Give the focus back to the last focus place when it is nowhere (on the page body)
    restoreFocus() {
      const last = this.lastFocus;
      const nowhere = !document.activeElement || document.activeElement === document.body;
      if (!last || !nowhere) return;
      if (last === "editor") this.placeSel(this.lastPlaced);
      else if (last.isConnected) last.focus();
    }

    // Leaving a language box without picking shows the current language again
    resetLangBox(box) {
      box.value = box.defaultValue;
      delete box.dataset.typed;
      this.send({ t: "lang-blur" });
    }

    // The user moved on from the editor: the kept focus place is dropped and a language box being
    // typed in is reset. What the cursor's last place shows (opened markers, the code block's
    // language box) stays: it follows only the cursor moving within the editor.
    leaveFocus() {
      const last = this.lastFocus;
      this.lastFocus = null;
      if (last && last !== "editor") this.resetLangBox(last);
      this.send({ t: "focus", on: false });
    }

    report() {
      // Typing in the language box does not move the editor's cursor
      if (document.activeElement && document.activeElement.matches(".zlang-input")) return;
      if (this.homeEnd) this.finishHomeEnd();
      if (this.vertical) this.finishVertical();
      // A selection outside the editor leaves what the cursor's last place shows
      const sel = this.readSel();
      if (!sel) return;
      this.markActiveCode();
      if (!this.pressed) this.applyReveal();
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

    // --------- Input ---------

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
      // The model replaces the selected content: native deletion cannot leave the hidden markers
      // behind
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
        // Ctrl + Enter in a code block leaves it (instead of sending or a new line)
        if (ctrl && !shift && !td && !this.opts.source && caret && inCodeBlock(caret.line)) return cmd("code-exit");
        const send = this.opts.enterSends ? !ctrl && !shift : ctrl && !shift;
        if (!td && caret && getSelection().isCollapsed) {
          const text = serialize(caret.line);
          if (caret.rel === text.length && OPEN_FENCE.test(text)) return cmd(send ? "fence-send" : "fence-newline");
        }
        if (send) return cmd("send");
        if (!td && caret && markupLength(caret.line) > 0) return cmd("newline");
        // Line breaks are normalized to paragraphs; serialization only recognizes blocks
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
      // Around the hidden fence lines of a code block: Backspace at the start of the line after the
      // block goes back into it (the line break is kept), and Delete never joins a fence line to
      // the line before it
      if (plain && getSelection().isCollapsed) {
        const closing = (n) => n && n.matches(".zl-close") && n.querySelector(".zlang");
        const prev = caret.line.previousElementSibling, next = caret.line.nextElementSibling;
        if (key === "Backspace" && caret.rel === 0 && closing(prev)) return cmd("code-back");
        if (key === "Delete" && caret.rel === serialize(caret.line).length && (closing(next) || (next && next.matches(".zl-code-open")))) {
          e.preventDefault();
          return;
        }
      }
      // At the start of a code block (its first line follows the hidden opening fence): Backspace
      // removes the block's two fences together
      if (plain && key === "Backspace" && caret.rel === 0 && getSelection().isCollapsed
        && caret.line.previousElementSibling && caret.line.previousElementSibling.matches(".zl-code-open")) return cmd("code-unwrap");
      // The cursor is right after a list or quote marker: Backspace deletes the innermost marker
      if (plain && key === "Backspace" && caret.rel > 0 && caret.rel === markupLength(caret.line)) return cmd("unprefix");
      if (this.opts.source) return;
      this.onLineKey(e, caret, plain);
    }

    // Keys next to a table or quote: text must not merge into it; the line-start marker is one unit
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
        case "Home":
        case "End":
          // The browser stops next to the visible text; the move is finished in report
          this.homeEnd = e.key;
          break;
        case "ArrowLeft":
          if (this.stepIntoSpan(e, line, rel, true) || this.approachSpan(e, line, rel, true)) break;
          if (start && prev && prev.classList.contains("zt")) {
            e.preventDefault();
            this.enterTable(prev, true);
          }
          break;
        case "ArrowRight":
          if (this.stepIntoSpan(e, line, rel, false) || this.approachSpan(e, line, rel, false)) break;
          if (end && next && next.classList.contains("zt")) {
            e.preventDefault();
            this.enterTable(next, false);
          }
          break;
        case "ArrowUp":
          if (prev && prev.classList.contains("zt") && this.onEdgeRow(line, true)) {
            e.preventDefault();
            this.enterTable(prev, true);
          } else if (prev && prev.querySelector(".zlang-input") && this.onEdgeRow(line, true)) {
            // Up from below a code block reaches its language box first
            e.preventDefault();
            this.markActiveCode(prev);
            prev.querySelector(".zlang-input").focus();
          } else {
            this.vertical = line;
          }
          break;
        case "ArrowDown":
          if (next && next.classList.contains("zt") && this.onEdgeRow(line, false)) {
            e.preventDefault();
            this.enterTable(next, false);
          } else if (next && next.querySelector(".zlang-input") && this.onEdgeRow(line, false)) {
            // Down from the last line of a code block reaches its language box
            e.preventDefault();
            this.markActiveCode(next);
            next.querySelector(".zlang-input").focus();
          } else {
            this.vertical = line;
          }
          break;
      }
    }

    // The cursor is on the first or last displayed line of the element
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
        // Select the whole cell content
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
          // An empty cell goes back to the previous cell: in the same row right to left, then to
          // the last cell of the previous row; the first cell has no previous one: delete the table
          // if the whole table is empty, otherwise do nothing
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
      const button = e.target.closest("[data-act]");
      if (button) {
        const q = button.closest(".zq");
        this.send({ t: "act", act: button.dataset.act, q: q ? +q.dataset.q : 0 });
        return;
      }
      // Clicking a whole element such as a mention, emoji or image: select it
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

    // --------- Table column widths ---------

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
        // Vertical lines land on whole pixels
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
