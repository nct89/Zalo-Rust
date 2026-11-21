//! Local preview conversations. No Index.db and no live API.

/// One fake row for the GPUI shell preview (`ZALO_GPUI_PREVIEW=1`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreviewConv {
    /// Stable row id (not a Zalo uid).
    pub id: u64,
    /// Display name.
    pub name: &'static str,
    /// Last-message preview line.
    pub preview: &'static str,
    /// Time column (already formatted).
    pub time: &'static str,
    /// Unread badge count. Zero hides the badge.
    pub unread: u32,
    /// Group thread (header skips Phone).
    pub is_group: bool,
    /// Online dot on the avatar.
    pub online: bool,
}

/// Hardcoded `ui-state.json` `split: false` — tabs are All / Unread.
pub const DEFAULT_SPLIT: bool = false;

/// Preview threads. Id 4 is "My Cloud".
pub const PREVIEW: &[PreviewConv] = &[
    PreviewConv {
        id: 1,
        name: "Nhóm dự án",
        preview: "Mai: Tài liệu đã gửi",
        time: "09:41",
        unread: 2,
        is_group: true,
        online: false,
    },
    PreviewConv {
        id: 2,
        name: "Lan Anh",
        preview: "Bạn: Ok nhé",
        time: "Hôm qua",
        unread: 0,
        is_group: false,
        online: true,
    },
    PreviewConv {
        id: 3,
        name: "Hùng",
        preview: "Gặp chiều nay được không?",
        time: "08:12",
        unread: 1,
        is_group: false,
        online: false,
    },
    PreviewConv {
        id: 4,
        name: "My Documents",
        preview: "Ảnh đã lưu",
        time: "T2",
        unread: 0,
        is_group: false,
        online: false,
    },
];

/// Id of the "My Documents" preview row.
pub fn cloud_of_mine_id() -> u64 {
    PREVIEW
        .iter()
        .find(|row| row.name == "My Documents")
        .map(|row| row.id)
        .unwrap_or(4)
}

/// Look up a preview row.
pub fn conv(id: u64) -> Option<&'static PreviewConv> {
    PREVIEW.iter().find(|row| row.id == id)
}

/// Rows after the local unread filter.
#[allow(dead_code)]
pub fn visible(unread_only: bool) -> impl Iterator<Item = &'static PreviewConv> {
    PREVIEW
        .iter()
        .filter(move |row| !unread_only || row.unread > 0)
}

/// One local person/group on the Contacts page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreviewPerson {
    /// Display name.
    pub name: &'static str,
    /// Online dot.
    pub online: bool,
    /// Group row.
    pub is_group: bool,
}

/// Preview friends (1-1), A→Z.
pub const FRIENDS: &[PreviewPerson] = &[
    PreviewPerson {
        name: "Hùng",
        online: false,
        is_group: false,
    },
    PreviewPerson {
        name: "Lan Anh",
        online: true,
        is_group: false,
    },
];

/// Preview groups.
pub const GROUPS: &[PreviewPerson] = &[PreviewPerson {
    name: "Nhóm dự án",
    online: false,
    is_group: true,
}];

/// People listed on a Contacts tab.
#[allow(dead_code)]
pub fn people(groups: bool) -> &'static [PreviewPerson] {
    if groups {
        GROUPS
    } else {
        FRIENDS
    }
}

/// Open the matching preview thread, if any.
pub fn conv_id_by_name(name: &str) -> Option<u64> {
    PREVIEW
        .iter()
        .find(|row| row.name == name)
        .map(|row| row.id)
}

/// Default list tabs (`split: false`).
pub fn default_list_tabs() -> (&'static str, &'static str) {
    (
        crate::color::str_vi("STR_ALL", "Tất cả"),
        crate::color::str_vi("STR_UNREAD_LABEL", "Chưa đọc"),
    )
}

/// One local bubble in the preview thread. No live history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreviewMsg {
    /// Parent preview conversation.
    pub conv: u64,
    /// Right-side `--blue-message` when true.
    pub from_me: bool,
    /// Group sender label; ignored on 1-1.
    pub sender: &'static str,
    /// Bubble body.
    pub text: &'static str,
    /// `HH:mm` under the card.
    pub time: &'static str,
}

/// Fake history aligned with [`PREVIEW`] last-message lines.
pub const PREVIEW_MSGS: &[PreviewMsg] = &[
    PreviewMsg {
        conv: 1,
        from_me: false,
        sender: "Mai",
        text: "Tài liệu đã gửi",
        time: "09:40",
    },
    PreviewMsg {
        conv: 1,
        from_me: true,
        sender: "Bạn",
        text: "Ok, mình xem tối nay",
        time: "09:41",
    },
    PreviewMsg {
        conv: 2,
        from_me: false,
        sender: "Lan Anh",
        text: "Mai họp lúc 9 nhé",
        time: "20:14",
    },
    PreviewMsg {
        conv: 2,
        from_me: true,
        sender: "Bạn",
        text: "Ok nhé",
        time: "20:16",
    },
    PreviewMsg {
        conv: 3,
        from_me: false,
        sender: "Hùng",
        text: "Gặp chiều nay được không?",
        time: "08:12",
    },
    PreviewMsg {
        conv: 4,
        from_me: true,
        sender: "Bạn",
        text: "Ảnh đã lưu",
        time: "07:05",
    },
];

/// Bubbles of one preview conversation, oldest first.
pub fn messages(conv: u64) -> impl Iterator<Item = &'static PreviewMsg> {
    PREVIEW_MSGS.iter().filter(move |msg| msg.conv == conv)
}

/// `.chat-date` label for a preview thread.
pub fn thread_day(conv: u64) -> &'static str {
    if conv == 2 {
        crate::color::str_vi("STR_YESTERDAY_RAW", "Hôm qua")
    } else {
        crate::color::str_vi("STR_TODAY", "Hôm nay")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_stays_off() {
        assert!(!DEFAULT_SPLIT);
        let (first, second) = default_list_tabs();
        assert_eq!(first, "Tất cả");
        assert_eq!(second, "Chưa đọc");
        assert_ne!(first, crate::color::str_vi("STR_TAB_CONV_FOCUS", "Ưu tiên"));
        assert_ne!(second, crate::color::str_vi("STR_TAB_CONV_OTHER", "Khác"));
    }

    #[test]
    fn unread_filter_keeps_badges() {
        let all: Vec<_> = visible(false).collect();
        let unread: Vec<_> = visible(true).collect();
        assert_eq!(all.len(), 4);
        assert_eq!(unread.len(), 2);
        assert!(unread.iter().all(|row| row.unread > 0));
        assert_eq!(cloud_of_mine_id(), 4);
        assert!(conv(1).is_some_and(|row| row.is_group));
        for row in PREVIEW {
            assert!(
                messages(row.id).next().is_some(),
                "preview conv {} needs bubbles",
                row.id
            );
        }
        assert_eq!(thread_day(2), "Hôm qua");
        assert_eq!(thread_day(1), "Hôm nay");
        assert_eq!(FRIENDS.len(), 2);
        assert_eq!(GROUPS.len(), 1);
        assert_eq!(conv_id_by_name("Lan Anh"), Some(2));
    }
}
