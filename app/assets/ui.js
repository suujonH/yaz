// 只做 Rust 做不到的 DOM 集成：虚拟列表的同帧测量与滚动位置修正、Esc 转发、默认右键菜单屏蔽。
(() => {
  // WebView 的默认右键菜单只留给输入框（剪切、复制、粘贴）
  document.addEventListener("contextmenu", (e) => {
    const t = e.target;
    if (!(t instanceof Element) || !t.closest("input, textarea, [contenteditable=true]")) e.preventDefault();
  });

  const lists = new Map();
  const BOTTOM_SLOP = 4;
  const SCROLL_TO_BOTTOM_MS = 300;
  const REVEAL_LIMIT_MS = 300;

  // 虚拟列表的结构：滚动容器 > 上占位、若干 .vl-item[data-key]、下占位。
  // 高度与渲染范围由 Rust 管理；这里在绘制前把滚动位置修正回来，界面不跳动。
  class VirtualList {
    constructor(el, send, stick, startAtBottom, initial) {
      this.el = el;
      this.send = send;
      this.stick = stick;
      this.atBottom = startAtBottom;
      this.anchor = null; // { key, off }：视口顶端落在该条目内，距条目顶端 off
      this.pending = initial; // { key, align, extra }：滚动到某条目，等它渲染出来；extra 是视口顶端相对对齐位置的额外距离
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

    // 初始定位完成，显示内容
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

    // 二分查找视口顶端所在的条目
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

    // 视口落在占位区、条目渲染出来之后，或粘底时：更新锚点并告诉 Rust
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
    // 返回的 Promise 在 listDetach 后完成
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
    listScrollToBottom(id) {
      const list = lists.get(id);
      if (list) list.scrollToBottom();
    },
    // 输入框内部已经处理（preventDefault）的 Esc 不转发
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
