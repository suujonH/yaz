// Only DOM integration that Rust cannot do: same-frame measurement and scroll position correction
// for the virtual list, Esc forwarding, suppressing the default context menu, and moving the caret
// across the tags of a token input.
(() => {
  // Token input (settings, e.g. the topic title): text pieces are separate inputs with tags between
  // them. At the edge of a piece, Left/Right continue into the neighbouring piece, past the tag.
  // Done here because the decision needs the caret position before the browser moves it.
  document.addEventListener("keydown", (e) => {
    if (e.isComposing || e.shiftKey || e.ctrlKey || e.altKey || e.metaKey) return;
    if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
    const input = e.target;
    if (!(input instanceof HTMLInputElement) || !input.classList.contains("token-text")) return;
    if (input.selectionStart !== input.selectionEnd) return;
    const left = e.key === "ArrowLeft";
    if (left ? input.selectionStart !== 0 : input.selectionEnd !== input.value.length) return;
    let next = left ? input.previousElementSibling : input.nextElementSibling;
    while (next && !next.classList.contains("token-text")) {
      next = left ? next.previousElementSibling : next.nextElementSibling;
    }
    if (!next) return;
    e.preventDefault();
    next.focus();
    const at = left ? next.value.length : 0;
    next.setSelectionRange(at, at);
  });

  // The WebView's default context menu is kept only for input boxes (cut, copy, paste)
  document.addEventListener("contextmenu", (e) => {
    const t = e.target;
    if (!(t instanceof Element) || !t.closest("input, textarea, [contenteditable=true]")) e.preventDefault();
  });

  const lists = new Map();
  const BOTTOM_SLOP = 4;
  const SCROLL_TO_BOTTOM_MS = 300;
  const REVEAL_LIMIT_MS = 300;

  // Virtual list structure: scroll container > top spacer, some .vl-item[data-key], bottom spacer.
  // Heights and the rendered range are managed by Rust; here the scroll position is corrected
  // before paint so the UI does not jump.
  class VirtualList {
    constructor(el, send, stick, startAtBottom, initial) {
      this.el = el;
      this.send = send;
      this.stick = stick;
      this.atBottom = startAtBottom;
      this.anchor = null; // { key, off }: the viewport top falls inside that item, at distance off from the item's top
      this.pending = initial; // { key, align, extra }: scroll to an item and wait for it to render; extra is the additional distance from the aligned position to the viewport top
      this.bottomUntil = 0;
      this.observed = new Set();
      this.closed = new Promise((resolve) => (this.close = resolve));

      this.resize = new ResizeObserver((entries) => this.onResize(entries));
      this.mutation = new MutationObserver(() => this.onMutation());
      this.resize.observe(el);
      this.mutation.observe(el, { childList: true });
      this.onScrollBound = () => this.onScroll();
      el.addEventListener("scroll", this.onScrollBound, { passive: true });
      this.syncObserved();
      this.correct();
      this.report();
      if (this.pending) setTimeout(() => this.reveal(), REVEAL_LIMIT_MS);
      else this.reveal();
    }

    // Initial positioning is done; show the content
    reveal() {
      this.el.removeAttribute("data-vl-positioning");
    }

    item(key) {
      return this.el.querySelector(`:scope > .vl-item[data-key="${key}"]`);
    }

    syncObserved() {
      const seen = new Set();
      for (const child of this.el.children) {
        seen.add(child);
        if (!this.observed.has(child)) {
          this.observed.add(child);
          this.resize.observe(child);
        }
      }
      for (const child of this.observed) {
        if (!seen.has(child)) {
          this.observed.delete(child);
          this.resize.unobserve(child);
        }
      }
    }

    onMutation() {
      this.syncObserved();
      this.correct();
    }

    onResize(entries) {
      const sizes = [];
      for (const entry of entries) {
        const key = entry.target.dataset && entry.target.dataset.key;
        if (key !== undefined) sizes.push([key, entry.borderBoxSize[0].blockSize]);
      }
      if (sizes.length) this.send({ t: "m", sizes });
      this.correct();
      if (entries.some((entry) => entry.target === this.el)) this.report();
    }

    onScroll() {
      if (this.pending) return;
      if (performance.now() < this.bottomUntil) {
        this.correct();
        this.report();
        return;
      }
      const el = this.el;
      this.atBottom = el.scrollHeight - el.scrollTop - el.clientHeight <= BOTTOM_SLOP;
      this.anchor = this.measureAnchor();
      this.report();
    }

    // Binary search for the item containing the viewport top
    measureAnchor() {
      const kids = this.el.children;
      const top = this.el.getBoundingClientRect().top;
      let lo = 1;
      let hi = kids.length - 2;
      let found = null;
      while (lo <= hi) {
        const mid = (lo + hi) >> 1;
        const rect = kids[mid].getBoundingClientRect();
        if (rect.bottom > top) {
          found = { kid: kids[mid], rect };
          hi = mid - 1;
        } else {
          lo = mid + 1;
        }
      }
      if (!found || found.rect.top > top + this.el.clientHeight) return null;
      return { key: found.kid.dataset.key, off: top - found.rect.top };
    }

    correct() {
      const el = this.el;
      if ((this.stick && this.atBottom) || performance.now() < this.bottomUntil) {
        const max = el.scrollHeight - el.clientHeight;
        if (Math.abs(el.scrollTop - max) > 0.5) el.scrollTop = max;
        this.captureAnchor();
        return;
      }
      if (this.pending) {
        const kid = this.item(this.pending.key);
        if (!kid) return;
        const rect = kid.getBoundingClientRect();
        const view = el.getBoundingClientRect();
        const align = this.pending.align;
        const off = align === "center"
          ? (rect.height - el.clientHeight) / 2
          : align === "end"
            ? rect.height - el.clientHeight
            : this.pending.extra || 0;
        this.anchor = { key: this.pending.key, off };
        this.pending = null;
        el.scrollTop += rect.top - view.top + off;
        this.atBottom = el.scrollHeight - el.scrollTop - el.clientHeight <= BOTTOM_SLOP;
        this.reveal();
        this.report();
        return;
      }
      const kid = this.anchor && this.item(this.anchor.key);
      if (!kid) {
        this.captureAnchor();
        return;
      }
      const delta = kid.getBoundingClientRect().top - el.getBoundingClientRect().top + this.anchor.off;
      if (Math.abs(delta) > 0.5) el.scrollTop += delta;
    }

    // After the viewport lands in a spacer region, after items have rendered, or when stuck to the
    // bottom: update the anchor and tell Rust
    captureAnchor() {
      const next = this.measureAnchor();
      const changed = (next && next.key) !== (this.anchor && this.anchor.key);
      this.anchor = next;
      if (changed) this.report();
    }

    report() {
      const el = this.el;
      this.send({
        t: "s",
        top: el.scrollTop,
        view: el.clientHeight,
        bottom: this.atBottom,
        anchor: this.anchor ? this.anchor.key : null,
        off: this.anchor ? this.anchor.off : 0,
      });
    }

    scrollTo(key, align, extra) {
      this.bottomUntil = 0;
      this.atBottom = false;
      this.pending = { key, align, extra };
      this.correct();
    }

    scrollToBottom() {
      this.pending = null;
      this.anchor = null;
      this.atBottom = true;
      this.bottomUntil = performance.now() + SCROLL_TO_BOTTOM_MS;
      this.correct();
      this.report();
    }

    detach() {
      this.resize.disconnect();
      this.mutation.disconnect();
      this.el.removeEventListener("scroll", this.onScrollBound);
      this.close();
    }
  }

  window.zulipUi = {
    // The returned Promise resolves after listDetach
    listAttach(id, send, stick, startAtBottom, initial) {
      const el = document.getElementById(id);
      if (!el) return Promise.resolve();
      const list = new VirtualList(el, send, stick, startAtBottom, initial);
      lists.set(id, list);
      return list.closed;
    },
    listDetach(id) {
      const list = lists.get(id);
      if (list) {
        lists.delete(id);
        list.detach();
      }
    },
    listScrollTo(id, key, align, extra) {
      const list = lists.get(id);
      if (list) list.scrollTo(key, align, extra);
    },
    // Keep the item under the client y coordinate where it is while the list content changes
    // (until the user scrolls): it becomes the anchor the list corrects to
    listHoldAt(id, y) {
      const list = lists.get(id);
      if (!list) return;
      const view = list.el.getBoundingClientRect();
      let held = null;
      for (const kid of list.el.querySelectorAll(":scope > .vl-item")) {
        const rect = kid.getBoundingClientRect();
        if (rect.bottom > y || !held) held = { kid, rect };
        if (rect.bottom > y) break;
      }
      if (!held) return;
      list.pending = null;
      list.bottomUntil = 0;
      list.atBottom = false;
      list.anchor = { key: held.kid.dataset.key, off: view.top - held.rect.top };
    },
    listScrollToBottom(id) {
      const list = lists.get(id);
      if (list) list.scrollToBottom();
    },
    // An Esc already handled inside the composer (preventDefault) is not forwarded
    onEscape(send) {
      document.addEventListener("keydown", (e) => {
        if (e.key === "Escape" && !e.defaultPrevented) send("esc");
      });
      return new Promise(() => {});
    },
    selectInput(id) {
      const el = document.getElementById(id);
      if (el) {
        el.focus();
        el.select();
      }
    },
  };
})();
