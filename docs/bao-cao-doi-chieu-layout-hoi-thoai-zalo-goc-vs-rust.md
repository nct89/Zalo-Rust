# Báo Cáo Đối Chiếu Mã Nguồn Hội Thoại: Zalo PC Gốc (Electron) vs Zalo-Rust (GPUI)

Ngày lập: 2026-11-25  
Nguồn Zalo PC gốc đối chiếu:
- `/home/nct89/Desktop/Projects/zalo-linux-rust/giao-dien/01-goc/pc-dist/compact-app-pc.d6af5465eb38c618bfe0.css`
- `/home/nct89/Desktop/Projects/zalo-linux-rust/giao-dien/01-goc/pc-dist/compact-app-pc.d6af5465eb38c618bfe0.js`

Nguồn Rust GPUI đối chiếu:
- `zalo-linux/crates/zalo-gpui/src/shell.rs`
- `zalo-linux/crates/zalo-gpui/src/sidebar.rs`
- `zalo-linux/crates/zalo-gpui/src/chat.rs`
- `zalo-linux/crates/zalo-gpui/src/media.rs`
- `zalo-linux/crates/zalo-gpui/src/widgets.rs`
- `zalo-linux/crates/zalo-gpui/src/context_menu.rs`

---

## 1. Liệt kê toàn bộ mã nguồn bố cục hội thoại trong Zalo PC gốc

### 1.1. Khung 3 cột chính & Responsive Breakpoints

| Thành phần | Mã nguồn Zalo PC gốc (CSS / JS) | Nguyên tắc bố cục |
|---|---|---|
| Cột điều hướng + Danh sách hội thoại (`#sidebarNav`, `nav`) | `body { --nav-width: 408px; --right-side-width: 344px; }` · `nav { width: var(--nav-width); border-right: 1px solid var(--divider-bold); }` · `#resize-handle { left: calc(var(--nav-width) - 4px); }` | Có kích thước linh hoạt theo `--nav-width` (kéo được) và đổi chế độ theo chiều rộng cửa sổ |
| Ngưỡng Responsive (`COMPACT_LEVEL`) | `verifyCompactMode`: `t > 1224 → NORMAL`; `880 < t <= 1224 → SEMI_COMPACT`; `t <= 880 → COMPACT`; `t < 500 → superCompact`; `MIN_WIDTH_WINDOW_APP = 580` (`main.js SizeConstants.DEFAULT_MIN_WIDTH = 550`, `DEFAULT_MIN_HEIGHT = 680`) | **`> 1224 px` (`NORMAL`)**: hiện đủ Rail `64` + Danh sách `344` + Chat + Info `344`. **`881..1224 px` (`SEMI_COMPACT`)**: Rail `64` + Danh sách `344` + Chat (`>= 473 px`), Info chuyển sang overlay. **`<= 880 px` (`COMPACT`)**: `#sidebarNav.compact-mode { width: 100% }` và `#sidebarNav.compact-mode.show-chat { width: 64px }` + `main.compact-mode { display: none }` — **ẩn hẳn cột danh sách khi đang xem chat** (khung chat ăn trọn `width - 64 px`, ngay cả ở `580 px` tối thiểu khung chat vẫn rộng **`516 px`**!) và hiện nút quay lại `<` (`conv-back-btn`) ở góc trái Header để quay về danh sách hội thoại toàn chiều rộng |
| Khung hội thoại (`#messageViewContainer`) | `#messageViewContainer { flex: 1 1 auto; }` · `.message-view__scroll { position: relative; flex: 1 0 auto; contain: layout style size; }` · `.message-view__scroller__innerScroller { max-width: 100%; display: flex; flex-direction: column; justify-content: flex-end; contain: layout style; }` | Cách ly hoàn toàn layout + paint trong khung chat (`contain: layout style size`), không bao giờ cho con tràn sang cột danh sách hội thoại bên trái hay tràn mép phải |
| Cột thông tin hội thoại (`#chatInfo`) | `WIDTH_CHAT_INFO = 340` · `.chat-info { position: absolute; overflow: hidden; z-index: 3; width: 100%; height: 100%; }` | Tự động nổi overlay khi cửa sổ không đủ rộng |

### 1.2. Cấu trúc một hàng tin nhắn (`.chat-item` → `.chat-content` → `.chat-message-v2` → `.card`)

```text
.chat-item (flex row, padding-left: 16px, padding-right: 16px, word-break: break-word)
  ├─ [Avatar 40px] (chỉ hiện với tin đến, đầu block)
  └─ .chat-content (flex col, margin-left: 10px, max-width: calc(100% - 50px))
       └─ .chat-message-v2 (position: relative)
            └─ .message-action (position: relative)
                 ├─ .card / .img-msg-v2 / .card--group-photo (nội dung bong bóng)
                 └─ .floating-menu-wrapper (position: absolute; bottom: 8px)
                      ├─ --other: left: 100%; transform: translate(8px)
                      └─ --me:    transform: translate(calc(-100% - 8px))
```

Chi tiết từng quy tắc CSS gốc:
- `.chat-item { padding-left: 16px; padding-right: 16px; word-wrap: break-word; word-break: break-word; }`
- `.chat-item.me { flex-flow: row-reverse nowrap; }`
- `.chat-content { margin: 0 0 0 10px; max-width: calc(100% - 50px); }`
- `.chat-content.me { margin: 0 11px 0 0; }`
- `.chat-message { display: flex; justify-content: flex-start; align-items: flex-start; position: relative; }`
- `.chat-message-v2 { position: relative; }` · `.chat-message-v2 .message-action { position: relative; }`

### 1.3. Quy tắc kích thước cho từng loại thẻ tin nhắn (Card Width Rules)

| Loại tin nhắn | Quy tắc gốc trong `compact-app-pc.*.css` & `.js` | Ghi chú quan trọng |
|---|---|---|
| Tin chữ (`.card--text`) | `.card--text { min-width: 40px; max-width: calc(100% - 38px); }` · `.card { padding: 12px; border-radius: 8px; }` | Tự co theo `100%` container, không cố định một số px cứng |
| Ảnh đơn (`.img-msg-v2` / `.chat-message-picture`) | `.chat-message-picture { max-width: 100%; flex: 0 0 auto; }` · `.img-msg-v2 { max-width: 100%; }` · `--thumb-cw: ${P}px` · `min-width: 180px; min-height: 110px` | Không bao giờ vượt quá `100%` container |
| Album nhiều ảnh (`.card--group-photo` / `ZGroupPhotoSkeleton` / `media_group_layout`) | `ZGroupPhotoSkeleton`: `this.maxWidth = 280` (dynamic layout cũ) · `media_group_layout`: `max_width: 450, target_height: 180, spacing: 2`, lấy `containerWidth` để tính `positions` · `.card--group-photo__row__item { width: 90px; height: 90px; }` | Chiều rộng album luôn bám theo `containerWidth`, không bao giờ cố định cứng `400px` bất kể cửa sổ hẹp |
| Tệp tin (`.card--file`) | `.card--file { max-width: calc(100% - 137px); width: 376px; }` | **Điểm mấu chốt**: `width: 376px` nhưng **bắt buộc chừa `137px`** (`calc(100% - 137px)`) cho thanh nút nổi + lề! |
| Link preview (`.card--link` / `.link-message-v2`) | `.card--link { padding: 12px 0; }` · `.card--link-container { width: 100%; height: auto; }` | Tự co theo khung cha, ảnh thumb `width: 100%` |
| Danh thiếp / Vị trí / Video / Call | `.card--business { max-width: calc(100% - 24px); }` · `.card--location { max-width: calc(100% - 24px); }` · `.card--video { width: auto; max-width: 800px; }` | Luôn có trần tương đối `calc(100% - ...)` |
| Emoji / Sticker | `.card--emoji { min-width: 96px; max-width: 75%; }` | Giới hạn tối đa 75% chiều rộng vùng chat |
| Thanh nút thao tác nhanh khi hover (`.floating-menu-wrapper` & `.chat-message__actions`) | `.floating-menu-wrapper { bottom: 8px; column-gap: var(--space-xxs); display: flex; position: absolute; }` · `.floating-menu-wrapper--other { left: 100%; transform: translate(8px); }` · `.floating-menu-wrapper--me { transform: translate(calc(-100% - 8px)); }` · `.chat-message__actions { width: 116px; height: 24px; z-index: 2; }` · `.chat-message__actions.photo-group { width: 64px; }` · `.chat-message__actions.three-options { width: 92px; }` | Neo `position: absolute` vào sát thẻ tin nhắn, nằm ngoài dòng flow chính, và các thẻ tin đã chừa sẵn quỹ không gian (`~116px–137px`) để thanh nút không bị cắt mép phải hoặc đè sang cột trái |
| Reaction (`.message-reaction-container`) | `.message-reaction-container { position: absolute; bottom: -15px; right: 12px; }` · `.reaction-emoji-list { display: flex; border-radius: 20px; padding: 0 10px; }` | Neo góc dưới-phải của thẻ, popup reaction đặt trong overlay có tính biên cửa sổ |
| Tooltip (`.z-tooltip`) | `.z-tooltip { position: fixed; max-width: 250px; word-wrap: break-word; padding: 12px 16px; z-index: 125; }` + Popper `boundary` clamping | **Bắt buộc `max-width: 250px` và tự xuống dòng**, có clamp biên viewport để không bao giờ tràn ra ngoài cửa sổ hay kéo dài xuyên cột |
| Khung soạn tin & Header | `.chat-box-bar { display: flex; height: 40px; padding: 0 8px; justify-content: space-between; }` · `.header-title { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }` | Tiêu đề co gọn (`min-width: 0`) thay vì đẩy nút bên phải ra ngoài |

---

## 2. Đối chiếu trực tiếp với mã nguồn Zalo-Rust hiện tại — Nguyên nhân chắp vá & lỗi cắt nội dung

### Phân tích toán học trên chính ảnh chụp lỗi (cửa sổ rộng `960 px`)
- Tổng chiều rộng cửa sổ: `960 px`
- Rail trái (`MAIN_TAB_W`): `64 px`
- Cột danh sách hội thoại (`CONV_LIST_INNER_W`): `344 px` (cố định)
- **Chiều rộng vùng hội thoại còn lại**: `960 - 64 - 344 = 552 px`
- Trừ padding trái/phải của danh sách (`.px(px(16.))` × 2 = `32 px`): **chiều rộng khả dụng mỗi hàng chỉ còn `520 px`**.

Trong khi đó, mã nguồn Rust hiện tại cộng dồn các hằng số gán cứng:

| Vấn đề | Vị trí trong Rust hiện tại | Tại sao bị lỗi / chắp vá |
|---|---|---|
| **1. Không theo dõi chiều rộng vùng hội thoại** | `chat.rs` / `media.rs` (`BUBBLE_MAX = 360.0`, `BUBBLE_INNER = 336.0`, `PHOTO_MAX_W = 280.0`, `album_body::MAX_W = 400.0`, `FILE_CARD_W = 376.0`, `Link = 300.0`, `Poll = 308.0`) | Toàn bộ chiều rộng thẻ và chiều rộng wrap chữ đều là hằng số chết, **không hề biết khung chat đang rộng `552 px` hay `1000 px`**. |
| **2. Album 3 ảnh bị cắt mất nút `...` bên phải** | `chat.rs:5216` (`const MAX_W: f32 = 400.0`), `chat.rs:3169` (`.pr(px(48.))`), `chat.rs:2928` (`actions` đặt **in-flow** cạnh `content` chiếm `116 px`) | Tổng yêu cầu hàng tin đến: `50 (avatar) + 400 (album) + 116 (hover actions) + 48 (pr) = 614 px > 520 px` → **tràn +94 px**, cắt cụt nút `...` và nửa nút Chia sẻ. |
| **3. Padding vá tay `.pl(88)` / `.pr(48)` sai bản chất** | `chat.rs:3143` (`.pl(px(88.))`), `chat.rs:3169` (`.pr(px(48.))`) | Hàng tin gửi đi bị gán cứng `pl(88)` (thiếu so với `116 px` actions), còn hàng tin đến bị cộng dư `pr(48)` chồng lên `116 px` in-flow actions (= mất oan `164 px`). |
| **4. `hover_actions` đặt in-flow thay vì neo tuyệt đối như `.floating-menu-wrapper`** | `chat.rs:2928-2935`, `chat.rs:3219-3223` | Khác hoàn toàn Zalo PC (`.floating-menu-wrapper` là `position: absolute` neo vào thẻ tin nhắn). Đặt in-flow khiến thanh nút chiếm chỗ cố định kể cả khi ẩn và xô lệch toàn bộ hàng khi hẹp. |
| **5. Thiếu `.overflow_hidden()` trên `thread` và `message-view`** | `chat.rs:144-156`, `chat.rs:2231-2238` | Khi bất kỳ thẻ tin, nút hover hay popup nào vượt quá chiều rộng vùng chat, nó **tràn thẳng sang đè lên cột danh sách tên bên trái** hoặc bị cắt thô ở mép cửa sổ. |
| **6. `widgets.rs::Tip` (Tooltip) không có `max-width` và không xuống dòng** | `widgets.rs:13-30` | Zalo PC `.z-tooltip` ép `max-width: 250px; word-wrap: break-word;`. Bản Rust không có giới hạn chiều rộng nên tooltip dài (như danh sách `"Đã xem: A, B, C, ..."` hoặc nhãn dài) kéo một vệt ngang xuyên qua cột khác / tràn mép màn hình. |
| **7. Popup trong khung chat (`mention_popup`, `receipt_panel`, `sticker_panel`) gán cứng width** | `chat.rs:1922` (`w(300.)`), `chat.rs:6980` (`w(220.)`), `stickers.rs` (`PANEL_W = 360.0`) | Khi khung chat hẹp `< 380 px`, popup tràn ra ngoài biên khung chat. |

---

## 3. Phương án xử lý triệt để theo đúng kiến trúc Zalo PC

Thay vì chỉnh từng hằng số rời rạc, gom toàn bộ tính toán kích thước hội thoại vào **một nguồn chân lý duy nhất `ChatLayoutMetrics`** cập nhật mỗi frame từ `window.viewport_size().width` (trừ rail `64 px`, sidebar `344 px`, và info panel `340 px` nếu đang dock):

1. **Tính quỹ chiều rộng chuẩn theo đúng công thức Zalo PC**:
   - `row_inner_w = chat_pane_w - 32.0` (trừ `.chat-item` padding `16px × 2`)
   - `avatar_slot = 50.0` (`40px` avatar + `10px` gap — đúng `.chat-content { max-width: calc(100% - 50px) }`)
   - `action_reserve = 116.0` (`4 × 24px + 3 × 4px` nút + `8px` khoảng hở — đúng `.chat-message__actions` + `.floating-menu-wrapper`)
   - Khi `row_inner_w - avatar_slot - action_reserve >= 180.0`:
     - `content_max_w = row_inner_w - avatar_slot - action_reserve`
     - `actions_above = false` (thanh nút neo sát cạnh phải tin đến / cạnh trái tin đi y hệt `.floating-menu-wrapper`)
   - Khi cửa sổ siêu hẹp (`row_inner_w < 346.0`):
     - `content_max_w = (row_inner_w - avatar_slot).max(120.0)`
     - `actions_above = true` (thanh nút tự động neo phía trên góc thẻ tin, không bao giờ tràn ngang)
   - Khi đang bật chế độ chọn nhiều tin (`select_msgs.is_some()`): trừ thêm `40 px` cột checkbox và ẩn thanh hover actions (đúng Zalo PC).
2. **Mọi loại thẻ đều lấy trần từ `ChatLayoutMetrics` (không còn một con số gán cứng nào phá vỡ biên)**:
   - `bubble_max_w = 360.0.min(content_max_w)` · `bubble_inner_w = (bubble_max_w - 24.0).max(96.0)` (dùng cho wrap text, code block, text+link, link card)
   - `album_max_w = 400.0.min(content_max_w)` (tỷ lệ chiều cao cell co tương ứng để ảnh không bị méo)
   - `photo_max_w = 280.0.min(content_max_w)`
   - `file_card_w = 376.0.min(content_max_w)`
   - `link_card_w = 300.0.min(content_max_w)`
   - `poll_card_w = 308.0.min(content_max_w)`
   - `compact_card_w = 220.0.min(content_max_w)` (voice, call, contact card, location, ecard, receipt panel)
3. **Neo `.floating-menu-wrapper` tuyệt đối vào thẻ tin, bỏ hoàn toàn `.pl(88)` / `.pr(48)` vá tay**:
   - Tin đến: `absolute().bottom(px(8.)).left(relative(1.0)).ml(px(8.))` (hoặc `top(px(-28.)).left_0()` khi `actions_above`)
   - Tin đi: `absolute().bottom(px(8.)).right(relative(1.0)).mr(px(8.))` (hoặc `top(px(-28.)).right_0()` khi `actions_above`)
4. **Cách ly khung cột bằng `.overflow_hidden()`** trên `sidebar_column`, `thread` (`chat_column`), `message-view`, và thanh công cụ soạn tin `chat-box-bar-id`.
5. **Chuẩn hóa Tooltip (`widgets.rs::Tip`) theo `.z-tooltip`**: giới hạn `max_w(px(250.))` và tự wrap dòng (`media::wrap_ui_note(&text, 234.0)`), không bao giờ tạo thanh ngang dài tràn cột hay ra ngoài cửa sổ.
6. **Kiểm thử tự động & chụp ảnh thực tế** ở nhiều kích thước cửa sổ (`960×729` giống ảnh báo lỗi, `760×640` siêu hẹp, và `1280×800` rộng) để kiểm chứng 100% không cắt nội dung, không tràn sang sidebar.
