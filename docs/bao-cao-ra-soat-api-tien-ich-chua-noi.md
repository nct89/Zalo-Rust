# Báo cáo rà soát API, tiện ích và tính năng chưa nối từ Zalo gốc & Electron sang Zalo-Rust

- **Ngày rà soát**: 2026-11-25
- **Phiên bản Rust đối chiếu**: `Zalo-Rust v0.3.3` (`core/*` + `zalo-linux/crates/zalo-gpui`)
- **Nguồn đối chiếu**:
  - Zalo Desktop gốc `26.10.10` / `26.10.20` (`docs/doi-chieu-electron-rust.md`, `zalo-linux-rust/giao-dien/`)
  - Zalo Desktop mới nhất `26.11.12` (`docs/upstream-reports/zalo-26.11.12.md`)
  - 13 plugin Electron `zalo-linux-native` (`giao-dien/11-zalo-linux-native/plugins/`) & nguồn Electron `zalomulti`

---

## 1. Tóm tắt số liệu rà soát trực tiếp trên mã nguồn Rust

| Hạng mục | Đã nối / có trong Rust | Một phần (🟡) | Chưa nối (⬜) | Tổng nguồn gốc |
|---|---:|---:|---:|---:|
| **API HTTP (nền 26.10.10)** | 181 | 1 | 226 | 408 |
| **API HTTP mới phát hiện ở 26.11.12** | — | — | +284 (gồm AI dịch, quản lý thẻ, chặn chia sẻ...) | — |
| **Hàm `pub fn` đã viết trong `core/zalo-client` nhưng CHƯA nối UI `zalo-gpui`** | — | — | **~16 API nghiệp vụ thực tế** | 305 hàm `pub` |
| **Lệnh WebSocket** | 26 | 6 | 16 (6 mục ➖ ngoài phạm vi) | 54 |
| **Loại tin nhắn (`msgType`)** | 18 | 2 | 3 | 23 |
| **Khóa cài đặt tài khoản máy chủ** | 10 | 1 | 7 | 18 |
| **13 Plugin Electron `zalo-linux-native`** | 8 hoàn chỉnh | 3 một phần | 2 chưa có (`spellcheck`, `userscripts`) | 13 |

---

## 2. Nhóm 1 — API đã viết sẵn trong `core/zalo-client` nhưng CHƯA nối vào UI `zalo-gpui` (ưu tiên làm ngay)

Đây là nhóm có thể nối rất nhanh vì tầng HTTP/crypto/payload đã có sẵn trong `core/zalo-client`:

| # | Hàm trong `core/zalo-client` | Vị trí mã nguồn | Chức năng gốc | Tình trạng trên `zalo-gpui` |
|---|---|---|---|---|
| 1 | `update_auto_delete_conversation` | `core/zalo-client/src/features.rs:1224` | Bật/tắt & chọn thời gian **Tin nhắn tự xóa** cho hội thoại | Hiện mới đọc danh sách (`getAutoDeleteConvers`), chưa có mục chọn TTL trong `info.rs` |
| 2 | `add_poll_options` | `core/zalo-client/src/features.rs:1028` | **Thêm phương án** vào bình chọn đang mở | `board.rs` / `chat.rs` mới tạo + vote bình chọn, chưa cho thêm lựa chọn mới |
| 3 | `reset_hidden_chats` | `core/zalo-client/src/api.rs:1579` | **Đặt lại mã PIN** & xóa danh sách trò chuyện ẩn (khi quên PIN) | `settings.rs` / `privacy.rs` mới có đặt/đổi/nhập PIN, chưa có nút "Quên mã PIN / Đặt lại" |
| 4 | `media_store_list` & `media_store_remove` | `core/zalo-client/src/api.rs:3348, 3427` | Tải thêm (phân trang) và **xóa ảnh/video/file** trong Kho phương tiện hội thoại | `info.rs` mới tải trang đầu preview, chưa có nút "Xem tất cả" phân trang & xóa từ kho |
| 5 | `list_catalog_page` | `core/zalo-client/src/features.rs:1133` | Phân trang danh mục sản phẩm zBusiness | `rail.rs` (`catalog_panel`) mới lấy 1 trang và chỉ hiển thị tên |
| 6 | `get_biz_card` | `core/zalo-client/src/api.rs:1337` | Lấy thông tin hồ sơ **Zalo Business** | Chưa hiển thị huy hiệu / thẻ Business trong `account.rs` |
| 7 | `get_community_quota` | `core/zalo-client/src/features.rs:1193` | Kiểm tra hạn mức trước khi nâng cấp nhóm lên Cộng đồng | Chưa kiểm tra & hiển thị hạn mức trong dialog nâng cấp cộng đồng |
| 8 | `verify_friends` | `core/zalo-client/src/api.rs:1252` | Xác minh quan hệ bạn bè với tập UID | Chưa nối để đồng bộ trạng thái người lạ / bạn bè chuẩn hơn |
| 9 | `send_hold_request`, `send_hold_response`, `send_unhold_request`, `send_unhold_response` | `core/zalo-client/src/voicecall.rs:867-897` | **Tạm giữ cuộc gọi (Hold / Unhold)** | `call.rs` chưa có nút giữ cuộc gọi và xử lý trạng thái Hold |
| 10 | `group_add_user`, `group_join_call`, `group_call_state`, `group_notify_call` | `core/zalo-client/src/voicecall.rs:926-959` | Signaling **Gọi nhóm** | Đã có phần gọi HTTP signaling, thiếu tầng media SRTP/multi-peer trong `zcall-core` & UI gọi nhóm trong `call.rs` |

---

## 3. Nhóm 2 — API HTTP từ Zalo gốc (26.10.10 / 26.10.20 / 26.11.12) CHƯA có trong Rust

### 3.1. Ưu tiên P0 — Chat, Media, Chuyển tiếp & Đồng bộ trạng thái cốt lõi

| Nhóm | Đường dẫn API | Mã lệnh / Hàm gốc | Chức năng & Tác động người dùng |
|---|---|---|---|
| **Gửi / Thu hồi / Chuyển tiếp hàng loạt** | `/api/message/multi_undo`, `/api/group/multi_undo` (`/api/group/multi_undomsg`) | `12115`, `12116` (`undoMultiMessage`) | Thu hồi nhiều tin nhắn đã chọn cùng lúc |
| | `/api/message/mforward`, `/api/group/mforward` | `12440`, `12442` (`forwardMultiMessage`) | Chuyển tiếp nhiều tin nhắn cùng lúc trong chế độ "Chọn nhiều tin" |
| **Gửi tin nhắn giàu nội dung** | `/api/message/link`, `/api/group/sendlink` | `11286`, `11193` (`sendLinkMessage`) | Gửi tin nhắn liên kết kèm thẻ preview chuẩn của Zalo (hiện đang gửi dạng text thường) |
| | `/api/group/mention` | `11198` (`sendMentionMessage`) | Gửi tin nhắn `@mention` nhóm chuẩn kèm metadata UID để điện thoại nhận thông báo nhắc tên |
| | `/api/message/photo_url`, `/api/group/photo_url` | `11775`, `11830` (`sendPhotoByUrl`) | Gửi nhanh ảnh từ URL/cache không cần upload lại |
| | `/api/message/renewlink`, `/api/message/scanurl` | `12094`, `12400` (`renewLink`, `scanUrl`) | Làm mới link CDN media hết hạn & quét cảnh báo link độc hại |
| **Upload ảnh / voice chunk mới** | `/api/message/upload/photo`, `/api/group/upload/photo` | `12651`, `12652` | Kênh upload ảnh thế hệ mới (`up_file`) |
| | `/api/message/voice/upload`, `/api/group/voice/upload`, `/api/message/upthumb` | `12060`, `12063`, `11293` | Upload tin nhắn thoại dung lượng lớn theo chunk & thumbnail |
| **Trạng thái tin nhắn & chưa đọc** | `/api/message/getStatusMsg`, `/api/group/getStatusMsg` | `12016`, `12017` | Xem chi tiết ai đã nhận / ai đã xem từng tin nhắn (đặc biệt trong nhóm) |
| | `/api/preloadconvers/get-last-seen` | `12009` (`getLastSeen`) | Khôi phục chính xác vị trí "Đã xem" ngay khi mở app |
| | `/api/conv/getUnreadMark` | `12011` (`getMarkAsUnread`) | Đồng bộ danh sách hội thoại đang đánh dấu "Chưa đọc" từ thiết bị khác |
| **Lịch sử tin nhóm Cloud (`group_cloud_message`)** | `/api/cm/mget`, `/api/cm/getpath`, `/api/cm/rgetv2`, `/api/cm/pc/dl` | `12503`, `12504`, `12508` | Tải lịch sử tin nhắn nhóm cũ trên máy chủ & nhảy tới tin nhắn cũ (khắc phục việc `/api/group/history` trả 404) |

### 3.2. Ưu tiên P1 — Quản lý Nhóm, Bảng tin, Bạn bè, Thẻ phân loại & Hồ sơ

| Nhóm | Đường dẫn API | Hàm gốc | Chức năng cụ thể |
|---|---|---|---|
| **Quản lý thẻ phân loại (mới 26.11.12)** | `/api/convlabel/manage/add`, `update`, `delete`, `reorder` | Quản lý nhãn phân loại | Tạo, sửa tên/màu, xóa và sắp xếp thứ tự thẻ phân loại đồng bộ máy chủ (hiện `labels.rs` mới đọc `/api/convlabel/get` và gán local/update) |
| **Chặn chia sẻ hội thoại (mới 26.11.12)** | `/api/conv/blocksharing/getConvers`, `updateConvers` | Thiết lập bảo mật nhóm/chat | Nối công tắc **"Chặn chia sẻ"** (hiện thấy trên panel phải Zalo gốc, mục Thiết lập bảo mật) |
| **Nhóm nâng cao** | `/api/group/blockedmems/list`, `add`, `remove` | `getBlockedMembers`, `blockMember`, `unblockMember` | Xem & quản lý danh sách thành viên bị chặn vào nhóm |
| | `/api/group/link/ginfo`, `/api/group/link/join` | `getGroupInfoByLink`, `joinGroupByLink` | Xem trước thông tin và tham gia nhóm khi bấm vào link `zalo.me/g/...` |
| | `/api/group/invite/multi`, `/api/group/kickout/multi` | `inviteMemberMulti`, `kickoutMemberMulti` | Thêm hoặc mời 1 người vào/ra nhiều nhóm cùng lúc |
| | `/api/group/inv-box/inv-info`, `/api/group/analyse`, `/api/group/topic/remove`, `/api/group/quota/members-comm` | `getGroupInvitationInfo`, `getGroupAnalyze`, `removeGroupTopic` | Chi tiết lời mời nhóm, thống kê nhóm, xóa chủ đề nhóm |
| **Bảng tin, Ghim đa tin & Giao việc (`boards` / `group_board` / `friend_board`)** | `/api/board/pin/list`, `/api/board/pin/update`, `/api/board/multi_unpin`, `/api/board/listReminder`, `/api/board/oneone/update` | `getListPinTopics`, `reorderPinTopic`, `multiUnpinPinTopics` | Đồng bộ danh sách tin ghim nhiều mục, sắp xếp & bỏ ghim hàng loạt, cập nhật nhắc hẹn 1-1 |
| | `/api/friendboard/list`, `create`, `reorder`, `multi_unpin` | Ghim tin trong chat 1-1 | Ghim tin nhắn 1-1 lên máy chủ (hiện `pinned_msgs` 1-1 chỉ lưu local) |
| | `/api/board/personal/list`, `get`, `delete`, `/api/board/personal/todo/create|update|status|ding|dingack|verify` | Zalo To-Do (Giao việc cá nhân) | Toàn bộ tính năng Giao việc (9 API) |
| | `/api/poll/share` | `sharePoll` | Chia sẻ bình chọn sang hội thoại khác |
| **Bạn bè & Tìm kiếm** | `/api/friend/search/by-user-name` | `getUserByUsername` | Tìm bạn bè theo `@username` (hiện chỉ tìm theo số điện thoại) |
| | `/api/friend/mget-qr` | `getQRProfileUser` | Lấy mã QR hồ sơ bạn bè / bản thân từ máy chủ |
| | `/api/friend/group/related` | `getRelatedGroup` | Hiển thị danh sách **Nhóm chung** trong Thông tin hội thoại 1-1 |
| | `/api/friend/closefr/update`, `/api/social/friend/getclosedfriends` | `updateCloseFriend`, `getCloseFriends` | Quản lý danh sách Bạn thân |
| | `/api/friend/feed/block` | `updateBlockStoriesStatus` | Chặn xem Nhật ký / Khoảnh khắc |
| | `/api/friend/reqstatus`, `/api/friend/recommendsv2/remove`, `/api/friend/recommendsv2/read-requests` | Quản lý lời mời & gợi ý kết bạn | Xóa gợi ý, đánh dấu đã đọc lời mời kết bạn |
| | `/api/recentsearch/list`, `/api/recentsearch/update`, `/api/alias/list/v2` | Tìm kiếm gần đây & biệt danh v2 | Đồng bộ lịch sử tìm kiếm gần đây trên thanh search |
| **Hồ sơ, Hiện diện & OA** | `/api/social/profile/getmute` | `getMuteConversations` | Đồng bộ danh sách hội thoại đang **Tắt thông báo** từ máy chủ khi đăng nhập |
| | `/api/social/profile/ping`, `deactive`, `changefgtobg`, `mode`, `updatelang`, `status` | Trạng thái hoạt động & hồ sơ | Báo trạng thái nền/tiền cảnh, đồng bộ ngôn ngữ lên máy chủ |
| | `/api/social/avatar-list`, `del-avatars`, `reuse-avatar`, `/api/social/profile/avatar` | Quản lý lịch sử ảnh đại diện | Xem/chọn lại hoặc xóa avatar cũ |
| | `/api/social/oa/block`, `/api/social/oa/unblock`, `/api/social/oa/convertid` | Chặn/bỏ chặn Official Account | Quản lý tin nhắn từ OA |
| | `/api/report/abuse-v2` | `reportUserV2` | Báo xấu theo chuẩn API v2 |
| | `/api/login/loginByPassw`, `/api/update/checkHasPassword`, `/api/update/password`, `/api/feedback` | Đăng nhập mật khẩu & phản hồi | Nối mục "Đăng nhập với số điện thoại" (hiện đang disabled ở `login.rs`) |

### 3.3. Ưu tiên P2 — Công cụ zBusiness, Sticker/GIF/Hiệu ứng, Reaction & Tiện ích

| Nhóm | Đường dẫn API | Chức năng cụ thể |
|---|---|---|
| **Danh mục sản phẩm zBusiness (`catalog` 10/11)** | `/api/prodcatalog/catalog/register|create|update|delete`, `/api/prodcatalog/product/list|view|create|update|delete|mdelete`, `/api/product/upload/photo` | Hiện chỉ xem danh sách tên danh mục; thiếu xem chi tiết sản phẩm, gửi thẻ sản phẩm vào chat, tạo/sửa/xóa sản phẩm |
| **Reaction đã nhận/đã xem** | `/api/message/react_delivered`, `/api/group/react_delivered`, `/api/message/react_seen`, `/api/group/react_seen`, `/api/react/gLastSn` | Đồng bộ badge thông báo thả cảm xúc mới (không báo lại cảm xúc đã xem) |
| **Sticker / GIF / Hiệu ứng** | `/api/message/effects`, `/api/message/effect/get`, `/api/message/sticker/suggest/keywords`, `/api/message/sticker/suggestion`, `/api/message/sticker/ai/list`, `/api/message/sticker/tenor/mapping`, `/api/message/guggy/default|search` | Gợi ý sticker theo từ khóa gõ trong ô soạn tin, hiệu ứng tin nhắn toàn màn hình, Sticker AI |
| **Thẻ ngân hàng & Sync Action** | `/api/transfer/card`, `/api/sync_action/media` | Gửi nhanh thẻ tài khoản ngân hàng (chuyển khoản QR) & đồng bộ thao tác media |

### 3.4. Ưu tiên P3 — Tính năng mới 26.11.12, E2EE & zCloud

| Nhóm | Đường dẫn API / Thành phần | Ghi chú kỹ thuật |
|---|---|---|
| **Zalo AI Dịch thuật & Voice-to-Text (mới 26.10.20 / 26.11.12)** | `/api/aifeatures/translate/v1/auto`, `manual`, `setting`, `suggest`, `/api/aifeatures/voice2text` | Gồm 60 khóa `STR_AI_TRANS_*` (dịch tự động hội thoại, dịch 1 tin nhắn, gợi ý dịch khi soạn, chuyển giọng nói sang văn bản) |
| **Mã hóa đầu cuối E2EE (`chat_e2e` 15 + `group_e2e` 16 = 31 API)** | `/api/e2ee/pc/message/*`, `/api/e2ee/pc/group/*` | Hiện tại khi gặp nhóm E2EE, Rust mới chỉ cảnh báo chưa mã hóa tin gửi đi; cần tích hợp phiên mã hóa Signal/zproto |
| **zCloud & LAN nội bộ** | `/api/message/zcloud-upload/photo`, `/api/group/zcloud-upload/photo`, `/api/lan/*` (5 API) | Lưu trữ đám mây zCloud (hiện mới mở link paywall trình duyệt) & truyền file/bạn bè qua mạng LAN |
| **Đồng bộ giao thức `26.11.12`** | `protocol.json` (`client_version` 694 → 697) + `assets/wasm/libzproto_wasm_bg` mới | Cần kiểm tra hash/export WASM trước khi nâng `client_version` |

---

## 4. Nhóm 3 — Lệnh WebSocket, Loại tin nhắn & Khóa cài đặt máy chủ chưa nối đủ

### 4.1. Lệnh WebSocket chưa nối / mới làm một phần
- **Chưa nối (`⬜`)**:
  - `4` (`PING_ACTIVE`): giữ trạng thái active khi cửa sổ đang focus (hiện chỉ gửi `PING = 2`).
  - `575` (`GET_STATUS_MSG`), `576` (`GET_STATUS_MSG_GROUP`), `577` (`GET_LAST_DELIVER_SEEN`), `578` (`GET_LAST_SEEN_CHAT_CONTAINER`): truy vấn trạng thái đã nhận/đã xem theo thời gian thực.
  - `614` (`CTRL_GET_LAST_SEEN_REACT`): đồng bộ mốc cảm xúc đã xem.
  - `530` / `536` / `537` (`*_INTERACTIVE_FILE`): thẻ tệp tương tác.
  - `532` (`VERIFY_MSG`), `620` / `621` / `623` (`PUSH` / `NEED_VERIFY` / `DONE_MIGRATE`), `800` (`GET_MEDIA_UPLOAD_URL`), `701` (`GET_IN_APP_PAYMENT_LINK`).
- **Mới làm một phần (`🟡`)**:
  - `602` (`CTRL_ACTION`): mới xử lý sự kiện đang soạn tin (`typing`), chưa xử lý đầy đủ sự kiện đang gửi tệp/ghi âm.
  - `601` / `603` (`CTRL_PUSH_CTRL` / `CTRL_PULL_CTRL`): mới đọc `transfer_sync2` và `voip`, chưa xử lý hết các `act_type` phụ (thu hồi đa thiết bị, cập nhật cài đặt, thu hồi thẻ phân loại).

### 4.2. Loại tin nhắn (`msgType`) chưa hiển thị đầy đủ
- `chat.doodle` (🟡): mới hiện nhãn văn bản, chưa vẽ ảnh doodle như `chat.photo`.
- `chat.location.new` (🟡): mới hiện nhãn vị trí, chưa render thẻ tọa độ + nút mở bản đồ.
- `chat.reaction` (⬜): tin nhắn hệ thống / tương tác cảm xúc dạng sự kiện riêng.
- `chat.e2ee.replace` (⬜): bản ghi thay thế nội dung sau khi giải mã E2EE.
- `group.leave` (⬜): sự kiện thành viên rời nhóm dạng `msgType` riêng.

### 4.3. Khóa cài đặt máy chủ (`getSettings` / `setSettings`) chưa nối
- `setting_show_stranger_msg_directly` — hiển thị trực tiếp tin nhắn từ người lạ.
- `archived_chat_status` — đồng bộ trạng thái mục lưu trữ / Khác.
- `quick_message_status` — đồng bộ bật/tắt gợi ý tin nhắn nhanh lên máy chủ (hiện chỉ lưu `quickOff` cục bộ).
- `show_seen` — đồng bộ ẩn/hiện trạng thái đã xem lên máy chủ (hiện `hideSeen` chỉ chặn gửi phía local).
- `numberOfOffload`, `popupApprearTime`, `enableOffload` — cấu hình tự dọn bộ nhớ đệm / offload media.

---

## 5. Nhóm 4 — Tiện ích & Plugin từ Electron (`zalo-linux-native` & `zalomulti`) và các mục còn thiếu trên UI GPUI

### 5.1. Đối chiếu 13 plugin Electron `zalo-linux-native`

| # | Plugin Electron | Hiện trạng trên `Zalo-Rust` | Việc cần bổ sung |
|---|---|---|---|
| 1 | `advanced-settings` | ✅ Đã tích hợp (`settings.rs`) | Bổ sung cài đặt Proxy & tùy chỉnh độ tối (`dark_tone`) |
| 2 | `launcher-badge` | ✅ Có trong `zalo-shell` | Kiểm tra tương thích đồng bộ số đếm trên GNOME/KDE |
| 3 | `offline` | 🟡 Mới có banner mất mạng / reconnect | Thiếu **hàng đợi tin nhắn offline bền vững (SQLite queue)** để tự động gửi lại khi có mạng |
| 4 | `screenshot` | ✅ Có vùng chụp + overlay + phím tắt | Đã đủ X11/Wayland/Portal/macOS/Windows |
| 5 | `spellcheck` | ⬜ **Chưa có** | Kiểm tra chính tả tiếng Việt/Anh (Hunspell) trong ô soạn tin `line_input.rs` |
| 6 | `start-hidden` | ✅ Hỗ trợ cờ CLI & autostart | Kiểm tra thêm khay hệ thống khi ẩn từ đầu |
| 7 | `tray-badge` | ✅ Có trong `zalo-shell` | — |
| 8 | `tray-host` | ✅ Có trong `zalo-shell` | — |
| 9 | `userscripts` | ⬜ **Chưa có** | Quy tắc tùy biến / lọc tin nhắn hoặc plugin mở rộng người dùng |
| 10 | `wayland-titlebar` | ✅ Có CSD titlebar (`titlebar.rs`) | — |
| 11 | `window-state` | ✅ Lưu vị trí & kích thước cửa sổ | — |
| 12 | `zadark` | 🟡 Đã có Dark Mode nền đen/xám trung tính | Bổ sung tùy chỉnh sắc độ tối (`dark_tone`), kích cỡ chữ chat, độ rộng cột sidebar |
| 13 | `zcall` | 🟡 Đã có gọi thoại & video 1-1 + AEC/RNNoise | Thiếu **gọi nhóm (multi-peer SRTP)** và tính năng **giữ cuộc gọi (Hold)** |

### 5.2. Tiện ích từ nguồn `zalomulti` & mục UI đang còn stub/placeholder trong `zalo-gpui`
1. **Hỗ trợ Proxy HTTP/SOCKS5** (từ `zalomulti/linux/electron/core/proxy-bridge.js`): cấu hình proxy trực tiếp cho `reqwest` + WebSocket trong `zalo-client` và màn hình Cài đặt mạng.
2. **Đa hồ sơ / chuyển tài khoản (`--profile`)**: chạy song song nhiều phiên `XDG_CONFIG_HOME` / `XDG_DATA_HOME` độc lập với label phân biệt trên titlebar/tray.
3. **Thư viện hình nền chat mặc định**: `settings.rs` hiện mới có "Không ảnh nền" và "Chọn ảnh từ máy", thiếu bộ hình nền có sẵn của Zalo.
4. **Cửa sổ đăng nhập (`login.rs`)**: 3 mục menu ("Đăng nhập với số điện thoại", "Đăng nhập với Facebook", "Quốc gia") hiện đang bị khóa với tooltip *"Bản Linux chưa hỗ trợ mục này"*.
5. **Thanh định dạng văn bản phong phú (RTF)** & **nhiều lựa chọn tin đánh dấu**:
   - Đã chuyển **Tin đánh dấu** sang panel sidebar chuẩn Zalo PC (không còn câu ghi chú kỹ thuật), nhưng lưu ý phía máy chủ Zalo PC gốc cũng chỉ lưu cục bộ (`IndexedDB StarMessage`); có thể cân nhắc bổ sung xuất/nhập danh sách tin đánh dấu cùng gói sao lưu PC-to-PC (`pc_export.rs`).
6. **Đồng bộ tin nhắn nhanh (`quick.rs`)**: đã nối CRUD API máy chủ, cần nối thêm khóa `quick_message_status` vào `/api/setting/update` và thêm đếm `(N/M)` + thanh tiêu đề `"Quay về cài đặt"` khi mở từ Cài đặt giống Zalo gốc.

---

## 6. Lộ trình triển khai đề xuất (sau khi hoàn tất vấn đề 1 & 2)

- **Đợt 1 (Nhanh — 100% tái dùng API sẵn có & P0 chat)**:
  1. Nối các API đã có trong `core/zalo-client` lên UI `zalo-gpui`: Tin nhắn tự xóa (`update_auto_delete_conversation`), Thêm lựa chọn bình chọn (`add_poll_options`), Quên/Đặt lại PIN ẩn chat (`reset_hidden_chats`), Xem tất cả & xóa media trong kho (`media_store_list/remove`), Thẻ Business (`get_biz_card`), Giữ cuộc gọi (`hold/unhold`).
  2. Triển khai API chat P0: `@mention` nhóm chuẩn (`/api/group/mention`), gửi thẻ link (`/api/message/link`, `/api/group/sendlink`), chuyển tiếp & thu hồi nhiều tin (`/api/message/mforward`, `/api/group/mforward`, `multi_undo`), tải lịch sử nhóm từ Cloud (`/api/cm/mget`, `/api/cm/rgetv2`), đồng bộ tắt thông báo & chưa đọc từ server (`/api/social/profile/getmute`, `/api/conv/getUnreadMark`, `/api/preloadconvers/get-last-seen`).
- **Đợt 2 (Quản trị nhóm, Bảng tin & Phân loại)**:
  1. Quản lý thẻ phân loại đồng bộ server (`/api/convlabel/manage/*`) & Chặn chia sẻ (`/api/conv/blocksharing/*`).
  2. Chặn thành viên nhóm, tham gia nhóm bằng link, xem nhóm chung, tìm bạn theo username, đồng bộ tìm kiếm gần đây.
  3. Ghim tin 1-1 (`friendboard`), sắp xếp/bỏ ghim nhiều tin nhóm (`board/pin/*`), chia sẻ bình chọn, vẽ chuẩn `chat.doodle` và thẻ bản đồ `chat.location.new`.
- **Đợt 3 (Tiện ích zBusiness, AI 26.11.12 & Chất lượng sống)**:
  1. Nâng `protocol.json` lên `26.11.12` (kiểm tra `libzproto_wasm_bg`), tích hợp **Zalo AI Dịch tin nhắn** (`/api/aifeatures/translate/v1/*`) & **Voice-to-Text** (`/api/aifeatures/voice2text`).
  2. Hoàn thiện **Danh mục sản phẩm zBusiness** (`catalog/*`, `product/*`) và **Giao việc To-Do** (`/api/board/personal/*`).
  3. Hàng đợi gửi tin offline bền vững (SQLite), kiểm tra chính tả tiếng Việt (Hunspell), Proxy HTTP/SOCKS5, và tùy chỉnh `dark_tone`.
- **Đợt 4 (Hạ tầng chuyên sâu)**:
  1. Gọi nhóm multi-peer SRTP (`zcall-core` + `voicecall.rs`).
  2. Hỗ trợ mã hóa đầu cuối E2EE (`chat_e2e` / `group_e2e`).
