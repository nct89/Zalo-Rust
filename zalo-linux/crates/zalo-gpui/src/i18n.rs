//! "Language › English".
//!
//! Most GPUI labels are written in Vietnamese in the code. [`t`] maps them
//! to English at render time: first through the Zalo catalogues
//! (Vietnamese text → language key → English, ~6 400 strings), then through
//! [`EXTRA`] for texts only this client has. Anything else stays Vietnamese.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

static ENGLISH: AtomicBool = AtomicBool::new(false);

/// Switch the UI language (Settings › Language).
pub fn set_english(on: bool) {
    ENGLISH.store(on, Ordering::Relaxed);
}

/// True while English is on.
pub fn is_english() -> bool {
    ENGLISH.load(Ordering::Relaxed)
}

/// GPUI-only texts (not in Zalo's catalogue).
const EXTRA: &[(&str, &str)] = &[
    ("Bật thông\nbáo", "Unmute"),
    ("Tắt thông\nbáo", "Mute"),
    ("Ghim hội\nthoại", "Pin"),
    ("Bỏ ghim\nhội thoại", "Unpin"),
    ("Thêm\nthành viên", "Add\nmembers"),
    ("Quản lý\nnhóm", "Manage\ngroup"),
    ("Tạo nhóm\ntrò chuyện", "Create\ngroup"),
    ("Tin\nđánh dấu", "Starred\nmessages"),
    ("Tin\nđồng thời", "Broadcast"),
    ("Trả lời\ntự động", "Auto\nreply"),
    ("Bản Linux chưa hỗ trợ mục này", "Not available on Linux yet"),
    ("Bấm lần nữa để rời nhóm", "Click again to leave"),
    ("Bấm lần nữa để xóa", "Click again to delete"),
    ("Bật quy tắc này", "Enable this rule"),
    ("Chia sẻ tin nhắn", "Share message"),
    ("Chưa có File được chia sẻ trong hội thoại này", "No files shared in this conversation"),
    ("Chưa có Link được chia sẻ trong hội thoại này", "No links shared in this conversation"),
    ("Chưa có Ảnh/Video được chia sẻ trong hội thoại này", "No photos/videos shared in this conversation"),
    ("Chưa có dữ liệu", "No data yet"),
    ("Chưa có thẻ phân loại", "No labels yet"),
    ("Chưa có tin trả lời tự động nào", "No auto replies yet"),
    ("Chưa gửi lời mời nào", "No sent requests"),
    ("Chưa hỗ trợ", "Not supported"),
    ("Chưa đăng nhập", "Not signed in"),
    ("Chưa đặt mã khóa", "No lock code set"),
    ("Chưa đặt — đặt khi ẩn trò chuyện đầu tiên", "Not set — set it when hiding the first chat"),
    ("Chỉ trả lời người lạ (không phải bạn bè)", "Only reply to strangers (not friends)"),
    ("Chọn một hội thoại để bắt đầu", "Select a conversation to start"),
    ("Chọn một hội thoại để gửi", "Select a conversation first"),
    ("Chọn thư mục lưu file", "Choose a download folder"),
    ("Chọn vị trí lưu trữ dữ liệu Zalo", "Choose where Zalo stores data"),
    ("Chọn ít nhất 2 người để tạo nhóm", "Pick at least 2 people to create a group"),
    ("Cloud của tôi", "My Documents"),
    ("My Documents", "My Documents"),
    ("Cuộc gọi", "Call"),
    ("Ứng dụng Zalo gốc viết bằng Rust & GPUI", "Native Zalo desktop client built with Rust & GPUI"),
    ("Phiên bản", "Version"),
    ("Bản dựng", "Build"),
    ("Giao thức", "Protocol"),
    ("Hệ điều hành", "Platform"),
    ("Nền tảng", "Engine"),
    ("Giấy phép", "License"),
    ("Mã nguồn", "Source code"),
    ("Thông tin phiên bản", "Version info"),
    (
        "Liên hệ hỗ trợ, báo lỗi hoặc đóng góp cho dự án Zalo Rust",
        "Contact support, report bugs, or contribute to Zalo Rust",
    ),
    ("Cuộc gọi nhóm", "Group call"),
    ("Cuộc gọi thoại đi", "Outgoing voice call"),
    ("Cuộc gọi thoại đến", "Incoming voice call"),
    ("Cuộc gọi thoại", "Voice call"),
    ("Bạn bị nhỡ", "You missed"),
    ("Người nhận bị nhỡ", "Missed by recipient"),
    ("Gọi lại", "Call back"),
    ("Cài đặt cuộc gọi", "Call settings"),
    ("Nâng cao", "Advanced"),
    ("Âm thanh cuộc gọi", "Call sounds"),
    ("Nhạc chuông", "Ringtone"),
    ("Nhạc chờ", "Ringback"),
    ("Mặc định của Zalo", "Zalo default"),
    ("Chọn tệp", "Choose file"),
    ("Nghe thử", "Preview"),
    ("Mặc định", "Default"),
    ("Đã lưu âm thanh. Áp dụng từ lần phát tiếp theo.", "Sound saved. It applies the next time it plays."),
    ("Đang nghe thử", "Playing a preview"),
    ("Đã dùng âm thanh mặc định của Zalo", "Using Zalo's default sound"),
    ("Không tìm thấy tệp âm thanh", "Sound file not found"),
    ("Chọn nhạc chuông", "Choose a ringtone"),
    ("Chọn nhạc chờ", "Choose a ringback tone"),
    ("Userscript Manager", "Userscript Manager"),
    ("Script DOM", "DOM scripts"),
    ("Chỉ bản Electron", "Electron only"),
    ("Kiểm tra chính tả", "Spell check"),
    ("Cần API máy chủ — bản GPUI chưa nối", "Needs a server API — not wired in GPUI yet"),
    ("Cắt", "Cut"),
    ("Dán", "Paste"),
    ("Dung lượng trống", "Free space"),
    ("Ghi chú, ghim, bình chọn", "Notes, pins, polls"),
    ("Ghi nhớ tài khoản đăng nhập", "Remember signed-in account"),
    ("Gọi thoại / video chưa hỗ trợ trên bản Linux", "Voice / video calls are not supported on Linux yet"),
    ("Gửi file", "Send file"),
    ("Gửi tin", "Send message"),
    ("Gửi ảnh", "Send photo"),
    ("Hai lần nhập không khớp", "The two entries do not match"),
    ("Hiện trò chuyện ẩn", "Show hidden chats"),
    ("Hội thoại đang mở tại cửa sổ riêng", "This conversation is open in its own window"),
    ("Không có lời mời vào nhóm", "No group invitations"),
    ("Kết thúc", "End"),
    ("Bắt đầu", "Start"),
    ("Nội dung", "Content"),
    ("Liên hệ đã chọn", "Selected contacts"),
    ("Liên kết", "Link"),
    ("Lặp lại hằng ngày (chỉ dùng giờ)", "Repeat daily (time only)"),
    ("Mã PIN", "PIN"),
    ("Mã PIN gồm đúng 4 chữ số", "The PIN must be exactly 4 digits"),
    ("Mã PIN không đúng", "Wrong PIN"),
    ("Mã khóa", "Lock code"),
    ("Mã khóa không đúng", "Wrong lock code"),
    ("Mở khóa", "Unlock"),
    ("Nhập PIN", "Enter PIN"),
    ("Nhập mã PIN", "Enter PIN"),
    ("Nhập lại mã PIN", "Repeat PIN"),
    ("Thiết lập mã PIN", "Set up PIN"),
    ("Thoại", "Voice"),
    ("Phó nhóm", "Deputy"),
    ("Trưởng nhóm", "Owner"),
    ("Thành viên", "Members"),
    ("Thành viên nhóm", "Group members"),
    ("Thêm thành viên", "Add members"),
    ("Danh sách thành viên", "Member list"),
    ("Thích", "Like"),
    ("Thời gian gửi", "Sent at"),
    ("Người gửi", "Sender"),
    ("Loại", "Type"),
    ("Kích thước", "Size"),
    ("Tên tệp", "File name"),
    ("Dung lượng", "Size"),
    ("Trên máy", "On this computer"),
    ("Trạng thái", "Status"),
    ("Đã gửi", "Sent"),
    ("Đã thu hồi", "Recalled"),
    ("Chi tiết tin nhắn", "Message details"),
    ("Tin trả lời tự động", "Auto reply message"),
    ("Tuỳ chọn khác", "More options"),
    ("Tệp tin", "File"),
    ("Tổng dung lượng", "Total size"),
    ("Vừa khung", "Fit"),
    ("Xóa lịch sử trò chuyện", "Delete chat history"),
    ("Zalo đang khóa. Nhập mã khóa để mở.", "Zalo is locked. Enter the lock code to open."),
    ("Đang bật — Ctrl + L để khóa", "On — Ctrl + L to lock"),
    ("Đang lưu…", "Saving…"),
    ("Đang tải…", "Loading…"),
    ("Đổi mã", "Change"),
    ("Đổi mã khóa", "Change lock code"),
    ("Đổi mã PIN", "Change PIN"),
    ("Đặt mã PIN", "Set PIN"),
    ("Tạo mã khóa", "Create lock code"),
    ("Tắt khóa màn hình", "Turn off screen lock"),
    ("Tạo mã khóa màn hình", "Create a screen lock code"),
    ("Tắt", "Turn off"),
    ("Mở", "Open"),
    ("Ảnh/Video", "Photos/Videos"),
    ("Ẩn trò chuyện", "Hide chat"),
    ("Bỏ ẩn trò chuyện", "Unhide chat"),
    ("Đổi tên nhóm", "Rename group"),
    ("Quản lý thẻ phân loại", "Manage labels"),
    ("Thẻ phân loại", "Label"),
    ("Màu thẻ", "Label colour"),
    ("Thêm thẻ", "Add label"),
    ("Sửa", "Edit"),
    ("Xóa", "Delete"),
    ("Hủy", "Cancel"),
    ("Đóng", "Close"),
    ("Lưu", "Save"),
    ("Quay lại", "Back"),
    ("Tạo mới", "New"),
    ("Xác nhận", "Confirm"),
    ("Đã chọn", "Selected"),
    ("Tìm bạn", "Search friends"),
    ("Tìm kiếm...", "Search..."),
    ("Lời mời đã nhận", "Received requests"),
    ("Lời mời đã gửi", "Sent requests"),
    ("Bạn không có lời mời nào", "You have no requests"),
    ("Từ chối", "Decline"),
    ("Đồng ý", "Accept"),
    ("Thu hồi lời mời", "Withdraw request"),
    ("Nhóm và cộng đồng", "Groups and communities"),
    ("Tên (A-Z)", "Name (A-Z)"),
    ("Tên (Z-A)", "Name (Z-A)"),
    ("Hoạt động (mới → cũ)", "Activity (newest first)"),
    ("Thu nhỏ", "Zoom out"),
    ("Phóng to", "Zoom in"),
    ("Lưu về máy", "Save to computer"),
    ("Danh sách nhắc hẹn", "Reminders"),
    ("Bảng tin nhóm", "Group board"),
    ("Thiết lập bảo mật", "Security settings"),
    ("Thông tin nhóm", "Group info"),
    ("Thông tin hội thoại", "Conversation info"),
    ("Rời nhóm", "Leave group"),
    ("Thiết lập", "Setup"),
    ("Tiếng Việt", "Tiếng Việt"),
    ("Phân loại", "Labels"),
    ("Thoát Zalo", "Quit Zalo"),
    ("Đồng bộ và sao lưu", "Sync and backup"),
    ("Thiết lập ẩn trò chuyện", "Hidden chats"),
    ("Đặt mã PIN cho các cuộc trò chuyện riêng tư để tránh người khác nhìn thấy trên máy của bạn", "Set a PIN for private chats so others on this computer cannot see them"),
    ("Đã đặt", "Set"),
    ("Nhập mã PIN để xem trong lần dùng này", "Enter the PIN to show them for this session"),
    ("Chưa có hội thoại", "No conversations yet"),
    (
        "Mất kết nối máy chủ tin nhắn, đang kết nối lại…",
        "Connection to messaging server lost, reconnecting…",
    ),
    (
        "Phiên này đã được mở ở nơi khác — ngừng nhận tin",
        "This session was opened elsewhere — realtime paused",
    ),
    ("Tìm kiếm", "Search"),
    (
        "Xuất tin nhắn văn bản đang có trên máy này ra file JSON. Không khôi phục từ điện thoại.",
        "Export text messages already on this computer to a JSON file. This does not restore from your phone.",
    ),
    ("Sao chép số", "Copy number"),
    ("Gọi qua Zalo", "Call on Zalo"),
    ("Kết bạn", "Add friend"),
    ("Giải tán nhóm", "Disband group"),
    ("Xác nhận giải tán nhóm", "Confirm disband group"),
    ("Nâng cấp thành cộng đồng", "Upgrade to community"),
    ("Xác nhận nâng cấp cộng đồng", "Confirm community upgrade"),
    ("Cộng đồng", "Community"),
    ("Chia sẻ", "Share"),
    ("Dừng chia sẻ", "Stop sharing"),
    ("Danh mục sản phẩm", "Product catalog"),
    ("Không ảnh nền", "No wallpaper"),
    ("Dùng avatar làm hình nền", "Use the avatar as the wallpaper"),
    ("Giấy", "Paper"),
    ("Sương", "Mist"),
    ("Mực nhạt", "Light ink"),
    (
        "Lưu trên máy này, giống Electron IndexedDB StarMessage. Không có API ghim sao lên máy chủ.",
        "Stored on this computer, like Electron's IndexedDB StarMessage. There is no server star API.",
    ),
    (
        "Máy chủ đánh dấu nhóm này là mã hóa đầu cuối. Máy này chưa mã hóa nội dung tin gửi đi.",
        "The server marks this group as end-to-end encrypted. This computer does not encrypt outgoing message text.",
    ),
    (
        "Máy tính không có API sửa hồ sơ doanh nghiệp. Đã mở trang Zalo Business.",
        "This computer has no API to edit the business profile. Opened the Zalo Business page.",
    ),
];

fn reverse() -> &'static HashMap<&'static str, &'static str> {
    static MAP: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut map: HashMap<&'static str, &'static str> = HashMap::new();
        for (key, vi) in zalo_design::VI {
            if let Some(en) =
                zalo_design::tr_en(key).filter(|e| !e.is_empty() && !e.contains("$0$"))
            {
                map.entry(vi).or_insert(en);
            }
        }
        // Client-specific texts win over catalogue guesses.
        for (vi, en) in EXTRA {
            map.insert(vi, en);
        }
        map
    })
}

/// Formats the localized member count string (e.g. "n members").
#[must_use]
pub fn members(n: impl std::fmt::Display) -> String {
    if is_english() {
        format!("{n} members")
    } else {
        format!("{n} thành viên")
    }
}

/// English for a Vietnamese UI text when English is on, else the text.
/// Counter suffixes like `"Friends (3)"` keep their numeric count.
#[must_use]
pub fn t(text: &str) -> String {
    if !is_english() {
        return text.to_owned();
    }
    lookup(text).unwrap_or_else(|| text.to_owned())
}

fn lookup(text: &str) -> Option<String> {
    let map = reverse();
    if let Some(en) = map.get(text) {
        return Some((*en).to_owned());
    }
    // «Title (n)» / «Title n/100»: translate the words, keep the count.
    if let Some((head, tail)) = text.rsplit_once(" (") {
        if tail.ends_with(')') {
            return map.get(head).map(|en| format!("{en} ({tail}"));
        }
    }
    if let Some((head, n)) = text.rsplit_once(' ') {
        if n.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            return map.get(head).map(|en| format!("{en} {n}"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_catalogue_and_extra() {
        // `lookup` only: the global language flag is shared by parallel tests.
        assert_eq!(
            lookup("Ghim hội thoại").as_deref(),
            Some("Pin this conversation")
        );
        assert_eq!(lookup("Mở khóa").as_deref(), Some("Unlock"));
        assert_eq!(
            lookup("Bạn bè (3)"),
            Some(format!("{} (3)", lookup("Bạn bè").unwrap()))
        );
        assert_eq!(lookup("không có trong bảng"), None);
        assert_eq!(t("Mở khóa"), "Mở khóa");
    }
}
