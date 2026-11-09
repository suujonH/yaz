//! 界面基础层。页面只依赖这里导出的组件和主题变量，用法见 `app/README.md`。

mod avatar;
mod controls;
mod flows;
mod geometry;
mod icon;
mod icon_data;
mod input;
mod layout;
mod menu;
mod overlay;
mod picker;
mod rows;
mod select;
mod virtual_list;

pub use avatar::{Avatar, OverlapStack};
pub use controls::{
    Badge, Button, ButtonKind, Checkbox, IconButton, RoleBadge, Slider, TabItem, Tabs, Toggle,
};
pub use flows::{Answer, Confirm, DialogButtons, TextPrompt, UnsavedBar};
pub use geometry::{Placement, Rect};
pub use icon::{Icon, IconName, icon_html};
pub use input::{InlineEdit, InputStyle, ShortcutRecorder, TextInput};
pub use layout::{
    CircleReveal, ExpandPanel, Expander, FileCard, HighlightText, SettingCard, SettingRow,
    SlideBar, TraceBorder,
};
pub use menu::{ContextMenuArea, MenuEntry};
pub use overlay::{
    DialogFrame, DialogHandle, OverlayHost, Overlays, Popover, PopoverHandle, PopoverOptions,
    ToastKind, Tooltip, use_overlays,
};
pub use picker::{Picker, PickerItem, PickerShell};
pub use rows::{ListRow, ProfileCard, ProfileField, RowDensity};
pub use select::{Select, SelectOption};
pub use virtual_list::{
    ListViewport, ScrollAlign, VirtualList, VirtualListConfig, VirtualListHandle, use_virtual_list,
};

use std::sync::atomic::{AtomicU64, Ordering};

/// 需要从 JS 定位元素时使用的唯一 ID。
fn next_dom_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("z{}", NEXT.fetch_add(1, Ordering::Relaxed))
}
