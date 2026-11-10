//! Protocol between the composer and the editor JS: messages received, content sent out, and
//! command dispatch.

use super::control::Ctl;
use super::core::{CellPos, EditorCore, Sel};
use crate::ui::Rect;
use crate::widgets::open_link;
use crate::windows;
use dioxus::prelude::*;
use serde::Deserialize;
use serde_json::{Value, json};
use zulip_core::markdown::format::FormatKind;
use zulip_core::markdown::table;
use zulip_core::offsets;

const EDITOR_JS: &str = include_str!("editor.js");

#[derive(Deserialize)]
#[serde(tag = "t", rename_all = "kebab-case")]
enum Incoming {
    Ready,
    Sync {
        seq: u64,
        rev: u64,
        text: String,
        s: usize,
        e: usize,
    },
    Cell {
        seq: u64,
        rev: u64,
        table: usize,
        row: usize,
        col: usize,
        value: String,
        s: usize,
        e: usize,
    },
    Sel {
        rev: u64,
        s: usize,
        e: usize,
        cell: Option<[usize; 3]>,
        x: f64,
        y: f64,
    },
    Cmd {
        cmd: EditorCmd,
    },
    Heading {
        level: u8,
    },
    Comp {
        on: bool,
    },
    Focus {
        on: bool,
    },
    Paste {
        text: String,
    },
    Insert {
        text: String,
    },
    File {
        id: u64,
        name: String,
        hex: String,
        last: bool,
    },
    TooBig,
    Act {
        act: QuoteAct,
        q: usize,
    },
    /// "More actions" of a table in an expanded quote.
    QuoteTable {
        q: usize,
        table: usize,
        x: f64,
        y: f64,
    },
    Tablerect {
        rect: Option<TableRect>,
    },
    /// Typing in the language box under a code block: `line` is the line of the opening fence,
    /// the rest is the box.
    LangQuery {
        line: usize,
        query: String,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    },
    /// A key in the language box: "up", "down", "enter" or "escape".
    LangKey {
        key: String,
    },
    /// The language box lost the focus.
    LangBlur,
    /// Ctrl + click on a link.
    OpenLink {
        href: String,
    },
    Widths {
        table: usize,
        required: Vec<f64>,
        minimum: Vec<f64>,
        available: f64,
    },
}

/// Commands sent from the editor. Unknown commands fail to parse and a warning is logged in the
/// message loop.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum EditorCmd {
    Copy,
    Cut,
    DeleteSelection,
    Undo,
    Redo,
    Send,
    CancelEdit,
    PopupUp,
    PopupDown,
    PopupAccept,
    PopupClose,
    Bold,
    Italic,
    Link,
    Code,
    Unprefix,
    /// Backspace at the start of a code block: remove its fences.
    CodeUnwrap,
    /// Backspace at the start of the line after a code block: back into the block.
    CodeBack,
    /// Ctrl + Enter in a code block: leave it for the line below.
    CodeExit,
    Newline,
    /// Enter at the end of the opening fence line (the key that sends / inserts a line break).
    FenceSend,
    FenceNewline,
    MergeUp,
    MergeDown,
    TableRowBelow,
    TableRowUp,
    TableRowDown,
    TableColLeft,
    TableColRight,
    TableDelRow,
    TableDelete,
}

/// Actions on the quote panel.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum QuoteAct {
    Toggle,
    Edit,
    EndEdit,
    Delete,
    Jump,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
pub struct TableRect {
    pub top: f64,
    pub left: f64,
    pub width: f64,
}

impl Ctl {
    /// Called after the editor is mounted.
    pub fn start(self) {
        let Some(mut rx) = self.rx.write_unchecked().take() else {
            return;
        };
        let id = self.dom_id.peek().clone();
        let script = format!(
            "{EDITOR_JS}; const ed = window.zulipComposer.attach({id:?}, (m) => dioxus.send(m)); for (;;) {{ ed.apply(await dioxus.recv()); }}"
        );
        self.spawn(async move {
            let mut eval = document::eval(&script);
            loop {
                tokio::select! {
                    message = eval.recv::<Value>() => match message {
                        Ok(message) => match serde_json::from_value::<Incoming>(message) {
                            Ok(message) => self.handle(message).await,
                            Err(error) => log::warn!("输入框消息无法解析：{error}"),
                        },
                        Err(error) => {
                            log::warn!("输入框脚本已结束：{error}");
                            return;
                        }
                    },
                    Some(message) = rx.recv() => {
                        let _ = eval.send(message);
                    }
                }
            }
        });
    }

    fn options(self) -> Value {
        let session = self.session();
        json!({
            "popup": self.popup.peek().is_some(),
            "source": self.core.peek().source,
            "enterSends": session.settings().enter_sends,
            "editing": self.is_editing(),
            "maxBytes": i64::from(session.realm().max_file_upload_size_mib) * 1024 * 1024,
        })
    }

    pub fn send_options(self) {
        self.out(json!({ "opts": self.options() }));
    }

    /// Give the editor focus again (after a menu or dialog closes).
    pub fn refocus(self) {
        self.out(json!({ "focus": true }));
    }

    // Send content to JS. `force` means the program changed the content; otherwise it is only a
    // reply to input
    pub(super) fn push(self, force: bool, focus: bool, keep_scroll: bool) {
        let core = self.core.peek();
        let mut message = json!({
            "blocks": core.blocks(),
            "rev": core.rev,
            "seq": *self.seq.peek(),
            "force": force,
        });
        if force && (focus || *self.focused.peek()) {
            let (s, e) = core.sel_utf16();
            message["sel"] = json!({
                "s": s,
                "e": e,
                "cell": core.sel.cell.map(|p| [p.table, p.row, p.col]),
            });
            message["focus"] = json!(focus);
        }
        if keep_scroll {
            message["keepScroll"] = json!(true);
        }
        drop(core);
        self.out(message);
    }

    fn push_reply_with_sel(self) {
        let core = self.core.peek();
        let (s, e) = core.sel_utf16();
        let message = json!({
            "blocks": core.blocks(),
            "rev": core.rev,
            "seq": *self.seq.peek(),
            "force": false,
            "sel": { "s": s, "e": e, "cell": core.sel.cell.map(|p| [p.table, p.row, p.col]) },
        });
        drop(core);
        self.out(message);
    }

    pub(super) fn out(self, message: Value) {
        let _ = self.tx.peek().send(message);
    }

    async fn handle(self, message: Incoming) {
        match message {
            Incoming::Ready => {
                self.send_options();
                self.push(true, true, false);
            }
            Incoming::Sync {
                seq,
                rev,
                text,
                s,
                e,
            } => {
                if rev != self.core.peek().rev {
                    return;
                }
                *self.seq.write_unchecked() = seq;
                let sel = Sel {
                    start: offsets::from_utf16(&text, s),
                    end: offsets::from_utf16(&text, e),
                    cell: None,
                };
                let moved = self.core.write_unchecked().sync(text, sel);
                if moved {
                    self.push_reply_with_sel();
                } else {
                    self.push(false, false, false);
                }
                self.after_change(true);
            }
            Incoming::Cell {
                seq,
                rev,
                table,
                row,
                col,
                value,
                s,
                e,
            } => {
                if rev != self.core.peek().rev {
                    return;
                }
                *self.seq.write_unchecked() = seq;
                let (start, end) = (
                    offsets::from_utf16(&value, s),
                    offsets::from_utf16(&value, e),
                );
                self.core.write_unchecked().sync_cell(
                    CellPos { table, row, col },
                    value,
                    start,
                    end,
                );
                self.push(false, false, false);
                self.after_change(true);
            }
            Incoming::Sel {
                rev,
                s,
                e,
                cell,
                x,
                y,
            } => {
                if rev != self.core.peek().rev {
                    return;
                }
                *self.caret.write_unchecked() = (x, y);
                let cell = cell.map(|[table, row, col]| CellPos { table, row, col });
                let sel = self.core.peek().sel_from_utf16(s, e, cell);
                let rerender = self.core.write_unchecked().set_sel(sel);
                if rerender {
                    self.core.write_unchecked().rev += 1;
                    self.push(true, false, false);
                }
                self.after_change(false);
            }
            Incoming::Cmd { cmd } => self.command(cmd),
            Incoming::Heading { level } => self.edit(|c| c.heading(level)),
            Incoming::Comp { on } => {
                *self.composing.write_unchecked() = on;
                if !on {
                    let deferred = std::mem::take(&mut *self.deferred.write_unchecked());
                    for change in deferred {
                        change(self);
                    }
                }
            }
            Incoming::Focus { on } => {
                *self.focused.write_unchecked() = on;
                if !on {
                    self.close_popup();
                }
            }
            Incoming::Paste { text } => self.paste_text(&text),
            Incoming::Insert { text } => self.edit(|c| c.insert(&text)),
            Incoming::File {
                id,
                name,
                hex,
                last,
            } => self.pasted_chunk(id, &name, &hex, last),
            Incoming::TooBig => self.too_big(),
            Incoming::Act { act, q } => self.quote_action(act, q),
            Incoming::QuoteTable { q, table, x, y } => {
                let html = self.core.peek().rendered_quote(q).map(str::to_owned);
                if let Some(html) = html {
                    let entries =
                        crate::pages::chat::table_menu_entries(self.overlays, &html, table);
                    self.overlays.show_menu(x, y, entries);
                }
            }
            Incoming::Tablerect { rect } => {
                let mut signal = self.table_rect;
                signal.set(rect);
            }
            Incoming::LangQuery {
                line,
                query,
                x,
                y,
                w,
                h,
            } => self.code_language_query(line, query, Rect { x, y, w, h }),
            Incoming::LangKey { key } => self.code_language_key(&key),
            Incoming::LangBlur => self.close_language_list(),
            Incoming::OpenLink { href } => {
                let (overlays, session) = (self.overlays, self.session());
                spawn(async move { open_link(overlays, &session, &href).await });
            }
            Incoming::Widths {
                table: index,
                required,
                minimum,
                available,
            } => {
                let cols = table::fit_columns(&required, available, Some(&minimum));
                self.out(json!({ "widths": { "table": index, "cols": cols } }));
            }
        }
    }

    fn command(self, cmd: EditorCmd) {
        match cmd {
            EditorCmd::Copy => self.copy(),
            EditorCmd::Cut => self.cut(),
            EditorCmd::DeleteSelection => self.edit(|c| c.insert("")),
            EditorCmd::Undo => {
                if self.core.write_unchecked().undo() {
                    self.push(true, true, false);
                    self.after_change(true);
                }
            }
            EditorCmd::Redo => {
                if self.core.write_unchecked().redo() {
                    self.push(true, true, false);
                    self.after_change(true);
                }
            }
            EditorCmd::Send => self.send(),
            EditorCmd::CancelEdit => {
                let mut editing = self.handle.editing;
                editing.set(None);
            }
            EditorCmd::PopupUp => self.move_popup(-1),
            EditorCmd::PopupDown => self.move_popup(1),
            EditorCmd::PopupAccept => self.accept_popup(None),
            EditorCmd::PopupClose => self.dismiss_popup(),
            EditorCmd::Bold => self.edit(|c| c.format(FormatKind::Bold)),
            EditorCmd::Italic => self.edit(|c| c.format(FormatKind::Italic)),
            EditorCmd::Link => self.edit(|c| c.format(FormatKind::Link)),
            EditorCmd::Code => self.edit(|c| c.format(FormatKind::Code)),
            EditorCmd::Unprefix => self.edit(EditorCore::remove_prefix),
            EditorCmd::CodeUnwrap => self.edit(EditorCore::unwrap_code_block),
            EditorCmd::CodeBack => self.edit(EditorCore::back_into_code),
            EditorCmd::CodeExit => self.edit(EditorCore::exit_code_block),
            EditorCmd::Newline => self.edit(EditorCore::newline),
            // Enter at the end of an opening fence line: complete it into a code block; elsewhere
            // handle Enter as usual
            EditorCmd::FenceSend | EditorCmd::FenceNewline => {
                let completed = self.core.write_unchecked().complete_fence();
                if completed {
                    self.push(true, true, false);
                    self.after_change(true);
                } else if cmd == EditorCmd::FenceSend {
                    self.send();
                } else {
                    self.edit(EditorCore::newline);
                }
            }
            EditorCmd::MergeUp => self.merge_blank_line(true),
            EditorCmd::MergeDown => self.merge_blank_line(false),
            EditorCmd::TableRowBelow => {
                self.edit(|c| c.table_op(|t, caret| t.insert_row(caret, true)));
            }
            EditorCmd::TableRowUp => self.edit(|c| c.table_op(|t, caret| t.move_row(caret, -1))),
            EditorCmd::TableRowDown => self.edit(|c| c.table_op(|t, caret| t.move_row(caret, 1))),
            EditorCmd::TableColLeft => {
                self.edit(|c| c.table_op(|t, caret| t.move_column(caret, -1)));
            }
            EditorCmd::TableColRight => {
                self.edit(|c| c.table_op(|t, caret| t.move_column(caret, 1)));
            }
            EditorCmd::TableDelRow => {
                self.edit(|c| c.table_op(|t, caret| Some(t.delete_row(caret))));
            }
            EditorCmd::TableDelete => {
                self.edit(|c| c.table_op(|t, _| Some(t.delete_table())));
            }
        }
    }

    // A blank line right next to a table: delete the line and put the cursor back in the table
    fn merge_blank_line(self, up: bool) {
        self.edit(|core| {
            let caret = core.sel.start;
            if up && caret > 0 {
                core.delete_range(caret - 1, caret);
            } else if !up {
                core.delete_range(caret, caret + 1);
            }
        });
    }

    fn quote_action(self, act: QuoteAct, index: usize) {
        match act {
            QuoteAct::Toggle => {
                self.core.write_unchecked().toggle_quote(index);
                self.push(true, false, false);
                self.render_quotes();
            }
            QuoteAct::Edit => {
                self.core.write_unchecked().edit_quote(index);
                self.push(true, true, false);
            }
            QuoteAct::EndEdit => {
                self.core.write_unchecked().end_quote_edit();
                self.push(true, true, false);
            }
            QuoteAct::Delete => self.edit(|c| c.delete_quote(index)),
            QuoteAct::Jump => {
                if let Some(id) = self.core.peek().quote_message_id(index) {
                    windows::open_chat(self.session().account_id(), self.key(), Some(id));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_parse_from_kebab_case() {
        let parse = |json: Value| serde_json::from_value::<Incoming>(json);
        for (name, expected) in [
            ("delete-selection", EditorCmd::DeleteSelection),
            ("cancel-edit", EditorCmd::CancelEdit),
            ("popup-accept", EditorCmd::PopupAccept),
            ("merge-up", EditorCmd::MergeUp),
            ("table-col-left", EditorCmd::TableColLeft),
            ("table-del-row", EditorCmd::TableDelRow),
            ("table-delete", EditorCmd::TableDelete),
            ("newline", EditorCmd::Newline),
            ("fence-send", EditorCmd::FenceSend),
            ("fence-newline", EditorCmd::FenceNewline),
        ] {
            let message = parse(json!({"t": "cmd", "cmd": name})).unwrap();
            assert!(
                matches!(message, Incoming::Cmd { cmd } if cmd == expected),
                "{name}"
            );
        }
        let message = parse(json!({"t": "act", "act": "end-edit", "q": 2})).unwrap();
        assert!(matches!(
            message,
            Incoming::Act {
                act: QuoteAct::EndEdit,
                q: 2
            }
        ));
        let message = parse(json!({"t": "heading", "level": 3})).unwrap();
        assert!(matches!(message, Incoming::Heading { level: 3 }));
        assert!(parse(json!({"t": "cmd", "cmd": "heading-1"})).is_err());
        assert!(parse(json!({"t": "act", "act": "nope", "q": 0})).is_err());
    }
}
