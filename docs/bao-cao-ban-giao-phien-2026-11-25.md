# Báo cáo bàn giao phiên làm việc Zalo-Rust (2026-11-25)

- **Dự án**: `/home/nct89/Desktop/Projects/Zalo-Rust` (nhánh `main`, nền commit `b53995e` — `v0.3.3`)
- **Trạng thái Git**: 41 tệp mã nguồn đã chỉnh sửa (+5.216 / -787 dòng, **chưa commit**) và 3 tài liệu báo cáo trong `docs/`.
- **Kiểm thử & biên dịch hiện tại**:
  - Lệnh chuẩn: `PATH="/home/nct89/.cargo/bin:$PATH" RUSTUP_TOOLCHAIN=1.100.0 cargo test -p zalo-gpui -- --test-threads=1`
  - Kết quả gần nhất: **233 passed, 0 failed, 0 warnings** (`target/debug/zalo-gpui` đã biên dịch sạch và đã xác minh trực quan trên cửa sổ chạy thật).
- **Tài liệu đối chiếu đi kèm**:
  - `docs/bao-cao-ra-soat-api-tien-ich-chua-noi.md` — Bảng rà soát toàn diện API, WebSocket, `msgType`, cài đặt server & 13 plugin Electron.
  - `docs/bao-cao-doi-chieu-layout-hoi-thoai-zalo-goc-vs-rust.md` — Đối chiếu nguyên bản CSS/JS layout hội thoại & responsive của Zalo PC gốc với bản Rust GPUI.

---

## 1. Các hạng mục đã hoàn thành & xác minh trong phiên này

### 1.1. Giao diện Dark Mode, màu sắc nội dung & Tin nhắn đánh dấu
- Chuẩn hóa toàn bộ **Dark Mode** về đen thuần & xám trung tính (`R == G == B`), loại bỏ hoàn toàn nền/viền/tô màu xanh dương trên bề mặt tối (tuân thủ nguyên tắc UI toàn cục).
- Chuẩn hóa **màu chữ liên kết / `@mention` / số điện thoại** trên nền tối: hiển thị chữ xanh `#66A6FF` (`color::link_fg()`), **bỏ gạch chân** trên liên kết.
- Thiết kế lại **Tin nhắn đánh dấu (Starred Messages)** theo bố cục cột sidebar chuẩn Zalo PC gốc.

### 1.2. Đồng bộ tin nhắn không gây đứng giao diện
- Tối ưu tiến trình đồng bộ `Sync 2` (`sync_worker.rs`) chạy nền theo từng lô (incremental), không khóa luồng UI khi đăng nhập lại hoặc đồng bộ lịch sử.

### 1.3. Nhóm 1 — Nối toàn bộ API có sẵn trong `core/zalo-client` lên UI `zalo-gpui`
Đã nối hoàn chỉnh lên giao diện và kiểm thử:
1. **Tin nhắn tự xóa (`update_auto_delete_conversation`)** — chọn TTL trong bảng Thông tin hội thoại (`info.rs`).
2. **Thêm lựa chọn bình chọn (`add_poll_options`)** — bổ sung phương án mới cho bình chọn đang mở (`board.rs`, `chat.rs`).
3. **Đặt lại mã PIN ẩn trò chuyện (`reset_hidden_chats`)** — nút "Quên mã PIN / Đặt lại" trong cài đặt bảo mật (`privacy.rs`, `settings.rs`).
4. **Kho phương tiện hội thoại (`media_store_list`, `media_store_remove`)** — xem tất cả có phân trang và xóa ảnh/video/file (`media_store.rs`, `info.rs`).
5. **Hồ sơ Zalo Business (`get_biz_card`)** — hiển thị thông tin/thẻ Business trong hồ sơ (`account.rs`).
6. **Hạn mức Cộng đồng (`get_community_quota`)** — kiểm tra và hiển thị hạn mức khi nâng cấp nhóm lên Cộng đồng (`dialogs.rs`, `info.rs`).
7. **Danh mục sản phẩm zBusiness (`list_catalog_page`)** — làm mới/tải trang danh mục trong panel công cụ (`rail.rs`).
8. **Tạm giữ cuộc gọi 1-1 (`send_hold_request`, `send_hold_response`, `send_unhold_request`, `send_unhold_response`)** — nút Hold/Unhold và đồng bộ trạng thái cuộc gọi (`call.rs`).

### 1.4. Nhóm 2A — API Chat P0 cốt lõi & sửa triệt để lỗi tin nhắn link trống
- **Đã triển khai tầng client (`core/zalo-client/src/ops.rs`, `api.rs`) và nối vào `zalo-gpui`**:
  - Gửi `@mention` nhóm chuẩn kèm metadata UID: `/api/group/mention` (`send_group_mention`).
  - Gửi tin nhắn liên kết chuẩn kèm preview metadata: `/api/message/link`, `/api/group/sendlink` (`send_link_message`), tuân thủ đúng cổng kiểm tra của Zalo PC gốc (`has_rich_link_metadata` — chỉ dùng `sendlink` khi `parselink` trả về tiêu đề/mô tả/ảnh hợp lệ; ngược lại gửi qua `mention`/`sendmsg`).
  - Thu hồi nhiều tin nhắn cùng lúc: `/api/message/multi_undo`, `/api/group/multi_undomsg` (`undo_multi_messages`).
  - Chuyển tiếp nhiều tin nhắn văn bản cùng lúc: `/api/message/mforward`, `/api/group/mforward` (`forward_multi_text`, chuẩn hóa `"msgType": "1"`).
  - Đồng bộ danh sách hội thoại tắt thông báo: `/api/social/profile/getmute` (`get_mute_conversations`).
  - Đồng bộ danh sách đánh dấu chưa đọc: `/api/conv/getUnreadMark` (`get_unread_marks`).
  - Khôi phục mốc "Đã xem" khi mở app: `/api/preloadconvers/get-last-seen` (`get_last_seen`).
  - Tải lịch sử tin nhóm từ Cloud: `/api/cm/mget`, `/api/cm/rgetv2` (`group_cloud_mget`, `group_cloud_jump`).
- **Sửa triệt để lỗi bong bóng gửi link bị trống** xuyên suốt 4 tầng (`bridge.rs`, `realtime.rs`, `shell.rs::merge_incoming_into_row`, `chat.rs`) và lưu `kind: "link"` trong storage (`store.rs`, `media.rs`).

### 1.5. Tái cấu trúc toàn bộ layout hội thoại & 3 chế độ co giãn chuẩn Zalo PC gốc
- **`ChatLayoutMetrics` (`chat.rs`)**: tính toán thống nhất bề rộng mọi bong bóng/album/thẻ/popup theo đúng mô hình hộp Zalo PC (`ROW_PAD_X = 32`, `AVATAR_SLOT = 50`, `ACTION_RESERVE = 116`, `SELECT_GUTTER = 40`).
- **Thanh nút thao tác nổi `.floating-menu-wrapper` (`chat.rs`)**: neo tuyệt đối theo bong bóng tin nhắn, không chiếm chỗ dòng chính và không bị cắt xén ở cửa sổ hẹp.
- **Tooltip chuẩn `.z-tooltip` (`widgets.rs`)**: giới hạn `max_w(250 px)` và tự xuống dòng, không tràn sang cột khác.
- **3 chế độ Responsive chuẩn `verifyCompactMode` (`shell.rs`, `sidebar.rs`, `chat.rs`, `rail.rs`)**:
  - `> 1224 px` → **`Normal`** (Rail + Danh sách + Chat + Panel phải gắn cạnh).
  - `881 – 1224 px` → **`SemiCompact`** (Rail + Danh sách + Chat; panel phải/tìm kiếm nổi đè).
  - `≤ 880 px` → **`Compact`** (ẩn xen kẽ cột Danh sách và cột Chat; ở chiều rộng tối thiểu `580 px`, khung chat có trọn `516 px`, header có nút `<` quay về danh sách toàn chiều rộng).
- **Giới hạn cửa sổ tối thiểu**: giữ sàn chiều rộng `580 px` (`JS_MIN_WIDTH_WINDOW_APP`), đã kiểm chứng trực quan qua ảnh chụp tại `960×729` và `580×680` (cả màn hình Chat, hover album 3 ảnh, màn hình Danh sách sau khi bấm `<`, và thử ép co xuống `420×320`).

### 1.6. Quy trình tự động cập nhật & cài đặt ngay trong ứng dụng cho cả 3 hệ điều hành
- **Động cơ cập nhật đa nền tảng (`updater.rs`)**:
  - Tự nhận diện hệ điều hành, kiến trúc CPU (`x86_64` / `aarch64`) và kiểu cài đặt hiện tại (`LinuxDeb` vs `LinuxPortable`, `MacOsAppBundle`, `WindowsInnoSetup` vs `WindowsPortable`) để chọn đúng gói phát hành và tệp `SHA256SUMS*.txt` tương ứng.
  - Tải gói phát hành dạng stream kèm thanh tiến trình byte/MB/% theo thời gian thực và kiểm tra toàn vẹn `SHA-256`.
  - Cài đặt & tự khởi động lại theo từng OS:
    - **Linux `.tar.gz` (user-local / portable)**: giải nén, thay thế nguyên tử `bin/zalo-gpui` + `share/zalo-rust` (không cần mật khẩu root) rồi tự mở lại ứng dụng.
    - **Linux `.deb` (hệ thống `/usr/bin/zalo-gpui`)**: cài qua `pkexec apt-get install -y --reinstall` (fallback `pkexec dpkg -i`) ngay trong lúc popup hiển thị trạng thái (hủy mật khẩu vẫn giữ app mở và báo lỗi rõ), sau đó tự khởi động lại.
    - **macOS (`Zalo.app` `.zip`)**: giải nén bằng `ditto`, gỡ `com.apple.quarantine`, chạy script nền đợi tiến trình cũ thoát rồi hoán đổi `Zalo.app` (hỗ trợ `osascript` admin nếu `/Applications` khóa quyền ghi) và chạy `open -n`.
    - **Windows (`-setup.exe` hoặc portable `.zip`)**: hỗ trợ cài thầm lặng Inno Setup (`/SILENT /SUPPRESSMSGBOXES /NORESTART /CLOSEAPPLICATIONS`) hoặc thay thế thư mục portable bằng script PowerShell nền rồi tự khởi động lại `zalo-gpui.exe`.
- **Popup thông báo & cài đặt ngay trong ứng dụng (`dialogs.rs`, `shell.rs`, `i18n.rs`)**:
  - Tự động bật popup `Dialog::UpdateAvailable` khi phát hiện phiên bản mới (hỗ trợ `"Bỏ qua phiên bản này"`, `"Để sau"`, `"Xem trên GitHub"`, `"Cập nhật & khởi động lại"` kèm thanh tiến trình trực tiếp, khóa đóng modal giữa chừng khi đang cài).
  - Bấm huy hiệu phiên bản trên thanh tiêu đề hoặc nút `"Cập nhật ngay"` trong mục Giới thiệu (`Dialog::About`) mở thẳng popup cài đặt tự động thay vì chỉ mở trình duyệt.
- **Đóng gói & CI**: bổ sung tạo/đẩy `SHA256SUMS-macos.txt` (`zalo-macos/packaging/build-app.sh`, `.github/workflows/release.yml`) và bật `CloseApplications=yes` + `RestartApplications=yes` trong `zalo-windows/packaging/installer.iss`.

---

## 2. Toàn bộ hạng mục & nội dung CHƯA CÓ trên bản Rust (Lộ trình cho phiên mới)

Quy ước triển khai: **làm từng nhóm một**, viết unit test đầy đủ, biên dịch 0 warning, mở đúng 1 bản `zalo-gpui` để kiểm tra trực quan rồi mới chuyển sang nhóm tiếp theo.

### Nhóm 2B (Ưu tiên tiếp theo ngay khi mở phiên mới) — P0 Chat, Media & Trạng thái tin nhắn còn lại
1. **Gửi ảnh bằng URL/cache**: `/api/message/photo_url`, `/api/group/photo_url` (`sendPhotoByUrl`).
2. **Làm mới link media hết hạn & quét link độc hại**: `/api/message/renewlink`, `/api/message/scanurl` (`renewLink`, `scanUrl`).
3. **Upload media theo kênh mới / chunk**:
   - Ảnh thế hệ mới (`up_file`): `/api/message/upload/photo`, `/api/group/upload/photo`.
   - Tin nhắn thoại dung lượng lớn & thumbnail: `/api/message/voice/upload`, `/api/group/voice/upload`, `/api/message/upthumb`.
4. **Chi tiết trạng thái từng tin nhắn (ai đã nhận / ai đã xem trong nhóm)**: `/api/message/getStatusMsg`, `/api/group/getStatusMsg` (kết hợp kiểm tra WS `575`/`576`).
5. **Hoàn thiện Cloud History nhóm còn lại**: `/api/cm/getpath`, `/api/cm/pc/dl`.

### Nhóm 3 (P1) — Thẻ phân loại, Chặn chia sẻ, Quản trị Nhóm nâng cao & Bảng tin
1. **Quản lý thẻ phân loại đồng bộ server (26.11.12)**: `/api/convlabel/manage/add`, `update`, `delete`, `reorder`.
2. **Chặn chia sẻ hội thoại (26.11.12)**: `/api/conv/blocksharing/getConvers`, `updateConvers` (công tắc "Chặn chia sẻ" trong Thiết lập bảo mật).
3. **Nhóm nâng cao**:
   - Thành viên bị chặn vào nhóm: `/api/group/blockedmems/list`, `add`, `remove`.
   - Xem trước & tham gia nhóm bằng link `zalo.me/g/...`: `/api/group/link/ginfo`, `/api/group/link/join`.
   - Mời / mời ra khỏi nhiều nhóm cùng lúc: `/api/group/invite/multi`, `/api/group/kickout/multi`.
   - Chi tiết lời mời nhóm, thống kê nhóm, xóa chủ đề: `/api/group/inv-box/inv-info`, `/api/group/analyse`, `/api/group/topic/remove`, `/api/group/quota/members-comm`.
4. **Bảng tin, Ghim đa tin & Giao việc (To-Do)**:
   - Ghim đa tin nhóm & nhắc hẹn 1-1: `/api/board/pin/list`, `update`, `/api/board/multi_unpin`, `/api/board/listReminder`, `/api/board/oneone/update`.
   - Ghim tin nhắn 1-1 đồng bộ server (hiện chỉ local): `/api/friendboard/list`, `create`, `reorder`, `multi_unpin`.
   - Chia sẻ bình chọn: `/api/poll/share`.
   - **Zalo To-Do (Giao việc cá nhân — 9 API)**: `/api/board/personal/list|get|delete`, `/api/board/personal/todo/create|update|status|ding|dingack|verify`.

### Nhóm 4 (P1) — Bạn bè, Tìm kiếm, Hồ sơ, Hiện diện & OA
1. **Tìm bạn theo `@username`**: `/api/friend/search/by-user-name` (hiện mới tìm theo SĐT).
2. **Mã QR hồ sơ từ server**: `/api/friend/mget-qr`.
3. **Danh sách Nhóm chung trong chat 1-1**: `/api/friend/group/related`.
4. **Bạn thân & Chặn Nhật ký**: `/api/friend/closefr/update`, `/api/social/friend/getclosedfriends`, `/api/friend/feed/block`.
5. **Lời mời & gợi ý kết bạn, xác minh bạn bè**: `/api/friend/reqstatus`, `/api/friend/recommendsv2/remove|read-requests`, nối `verify_friends` đã có sẵn trong `core/zalo-client`.
6. **Tìm kiếm gần đây & biệt danh v2**: `/api/recentsearch/list`, `update`, `/api/alias/list/v2`.
7. **Hiện diện, lịch sử avatar, OA, báo xấu & mật khẩu**:
   - `/api/social/profile/ping`, `deactive`, `changefgtobg`, `mode`, `updatelang`, `status`.
   - `/api/social/avatar-list`, `del-avatars`, `reuse-avatar`, `/api/social/profile/avatar`.
   - `/api/social/oa/block`, `unblock`, `convertid`.
   - `/api/report/abuse-v2`, `/api/feedback`.
   - Đăng nhập bằng mật khẩu/SĐT (bỏ khóa mục đang disabled ở `login.rs`): `/api/login/loginByPassw`, `/api/update/checkHasPassword`, `/api/update/password`.

### Nhóm 5 (P2) — zBusiness Catalog CRUD, Reaction, Sticker/GIF/Hiệu ứng & Thẻ ngân hàng
1. **Danh mục sản phẩm zBusiness đầy đủ (10 API)**: `/api/prodcatalog/catalog/register|create|update|delete`, `/api/prodcatalog/product/list|view|create|update|delete|mdelete`, `/api/product/upload/photo` + gửi thẻ sản phẩm vào chat.
2. **Đồng bộ trạng thái Reaction đã nhận / đã xem**: `/api/message/react_delivered`, `/api/group/react_delivered`, `/api/message/react_seen`, `/api/group/react_seen`, `/api/react/gLastSn`.
3. **Sticker gợi ý theo từ khóa, Sticker AI, Hiệu ứng toàn màn hình & GIF Guggy**: `/api/message/effects`, `/api/message/effect/get`, `/api/message/sticker/suggest/keywords`, `suggestion`, `ai/list`, `tenor/mapping`, `/api/message/guggy/default|search`.
4. **Thẻ tài khoản ngân hàng & Sync Action**: `/api/transfer/card`, `/api/sync_action/media`.

### Nhóm 6 — Lệnh WebSocket, Loại tin nhắn (`msgType`) & Khóa cài đặt server còn thiếu
1. **Lệnh WebSocket chưa nối / một phần**:
   - `4` (`PING_ACTIVE`) khi cửa sổ focus.
   - `575` (`GET_STATUS_MSG`), `576` (`GET_STATUS_MSG_GROUP`), `577` (`GET_LAST_DELIVER_SEEN`), `578` (`GET_LAST_SEEN_CHAT_CONTAINER`), `614` (`CTRL_GET_LAST_SEEN_REACT`).
   - `530` / `536` / `537` (`*_INTERACTIVE_FILE`), `532` (`VERIFY_MSG`), `620/621/623`, `800` (`GET_MEDIA_UPLOAD_URL`), `701` (`GET_IN_APP_PAYMENT_LINK`).
   - Mở rộng `602` (`CTRL_ACTION` — gửi tệp/ghi âm) và `601/603` (`CTRL_PUSH_CTRL`/`CTRL_PULL_CTRL` — các `act_type` đồng bộ đa thiết bị).
2. **Loại tin nhắn (`msgType`) chưa hiển thị đầy đủ**:
   - `chat.doodle` (vẽ ảnh doodle thay vì chỉ nhãn chữ).
   - `chat.location.new` (thẻ tọa độ + nút mở bản đồ).
   - `chat.reaction`, `chat.e2ee.replace`, `group.leave`.
3. **7 khóa cài đặt server (`getSettings` / `setSettings`) chưa nối**:
   - `setting_show_stranger_msg_directly`, `archived_chat_status`, `quick_message_status`, `show_seen`, `numberOfOffload`, `popupApprearTime`, `enableOffload`.

### Nhóm 7 — Tiện ích Desktop Linux / Electron & mục UI còn stub
1. **Hàng đợi gửi tin offline bền vững (SQLite queue)** — tự động gửi lại khi có mạng.
2. **Kiểm tra chính tả tiếng Việt / Anh (Hunspell)** trong `line_input.rs` (plugin `spellcheck`).
3. **Userscripts / quy tắc tùy biến người dùng** (plugin `userscripts`).
4. **Hỗ trợ Proxy HTTP/SOCKS5** cho `reqwest` + WebSocket và giao diện Cài đặt mạng.
5. **Đa hồ sơ / chuyển tài khoản (`--profile`)** với nhãn phân biệt trên titlebar & tray.
6. **Tùy biến giao diện (`zadark` mở rộng)**: chỉnh `dark_tone` (đen thuần / xám trung tính), cỡ chữ hội thoại, độ rộng cột danh sách.
7. **Thư viện hình nền chat mặc định** (hiện mới có "Không ảnh nền" và "Chọn từ máy").
8. **Tin nhắn nhanh (`quick.rs`)**: nối khóa `quick_message_status` lên server, thêm bộ đếm `(N/M)` và thanh `"Quay về cài đặt"`.
9. **Thanh định dạng văn bản (RTF)** & cân nhắc xuất/nhập Tin nhắn đánh dấu cùng gói sao lưu PC-to-PC (`pc_export.rs`).
10. **Cửa sổ đăng nhập (`login.rs`)**: kích hoạt lại Đăng nhập bằng SĐT/mật khẩu (và quốc gia).

### Nhóm 8 (P3) — Nâng cấp giao thức `26.11.12`, Zalo AI & Hạ tầng chuyên sâu
1. **Đồng bộ giao thức `26.11.12`**: nâng `protocol.json` (`client_version` `694 → 697`) sau khi kiểm tra hash/export `libzproto_wasm_bg`.
2. **Zalo AI Dịch thuật & Voice-to-Text**: `/api/aifeatures/translate/v1/auto|manual|setting|suggest`, `/api/aifeatures/voice2text`.
3. **Gọi nhóm multi-peer SRTP**: tầng media nhiều kết nối trong `zcall-core` + UI gọi nhóm trong `call.rs` (đã có phần HTTP signaling `group_add_user`, `group_join_call`, `group_call_state`, `group_notify_call` trong `voicecall.rs`).
4. **Mã hóa đầu cuối E2EE (31 API)**: `/api/e2ee/pc/message/*` (15 API) và `/api/e2ee/pc/group/*` (16 API) tích hợp phiên mã hóa Signal/zproto.
5. **zCloud & truyền tệp mạng LAN**: `/api/message/zcloud-upload/photo`, `/api/group/zcloud-upload/photo`, `/api/lan/*` (5 API).

---

## 3. Lưu ý chuyển tiếp sang phiên mới

1. **Khuyến nghị trước khi code tiếp**: cân nhắc `git commit` toàn bộ thay đổi đã xác minh của phiên này thành một mốc sạch (ví dụ `feat: dark mode neutral, sync2 non-blocking, group 1 & 2A APIs, Zalo PC 3-mode compact layout`) để dễ theo dõi diff của các nhóm sau.
2. **Quy tắc bắt buộc giữ nguyên ở phiên mới**:
   - Không dùng màu nền/tô/viền xanh dương trên Dark Mode (`R == G == B`), chỉ dùng `#66A6FF` không gạch chân cho chữ link/`@mention`/SĐT.
   - Mọi logic giao thức/layout đối chiếu trực tiếp mã nguồn Zalo PC gốc (tránh sửa chắp vá).
   - Luôn biên dịch với `RUSTUP_TOOLCHAIN=1.100.0`, giữ 0 warning, chỉ chạy **1 tiến trình `zalo-gpui`** duy nhất trên cùng tài khoản khi test thực tế.
3. **Câu lệnh mở đầu gợi ý cho phiên mới**:
   > *"Đọc `docs/bao-cao-ban-giao-phien-2026-11-25.md` và triển khai tiếp **Nhóm 2B** theo đúng quy trình kiểm thử và xác minh trực quan."*
