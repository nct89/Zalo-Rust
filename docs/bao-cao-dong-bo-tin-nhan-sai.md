# Báo cáo: mã đồng bộ tin nhắn trên Rust đã lệch Electron

Ngày đối chiếu: 2026-11-21.

Phạm vi của bản này là **đồng bộ tin nhắn từ điện thoại sang PC**. Hai việc khác không nằm trong lỗi của bản này:

- Sao lưu tin nhắn PC sang điện thoại đã bị gỡ ở lượt sau. Xem `docs/bao-cao-sao-luu-tin-nhan-sai.md`.
- Xuất / Nhập dữ liệu: file zip trên máy này. Phần này còn.

Ba việc đó không dùng chung một giao thức. Bản Rust trước đã viết một máy `drive()` riêng cho chiều điện thoại sang PC, rồi coi đó là Sync 2. Máy đó không khớp nguồn Electron. Mã đó đã được gỡ. Tài liệu này ghi lại hướng sai. File `docs/huong-trien-khai-dong-bo-tin-nhan.md` là hướng viết lại từ nguồn Electron, chưa phải mã mới.

## Việc đã gỡ

Đã xóa module `phone_sync` và các file phiên socket trong `core/zalo-sync2`: `batch.rs`, `keys.rs`, `message.rs`, `request.rs`, `session.rs`, `tests.rs`.

Đã gỡ chỗ bật tính năng:

- thẻ Cài đặt «Đồng bộ tin nhắn» (nút bắt đầu và nút Hủy của thẻ đó);
- banner gợi ý đồng bộ trên danh sách hội thoại;
- hộp thoại `SyncPrompt` sau đăng nhập;
- cờ `pendingSyncPrompt`, `syncBannerDismissed`;
- file tạm `sync-temp.key`, `sync-temp.at` trong gói xuất và khi gỡ tài khoản;
- helper HTTP `request_transfer_sync`, `pull_mobile_msg`, `cancel_pull_mobile_msg`, `get_crossdb`, `confirm_needs_sync`;
- nhánh chuyển tiếp sự kiện `transfer_status` vào worker đồng bộ;
- `BackupUi::blocks_sync`, không còn chỗ gọi sau khi thẻ đồng bộ bị gỡ.

Lúc viết báo cáo đồng bộ, trang Cài đặt còn hai thẻ: «Sao lưu tin nhắn» và «Xuất dữ liệu/ Nhập dữ liệu». Lượt sau đã gỡ thẻ sao lưu. Thẻ xuất/nhập còn.

`cargo test -p zalo-sync2 -p zalo-gpui --offline --lib` sau khi gỡ đồng bộ: `zalo-sync2` 3 test, `zalo-gpui` 189 test, 0 lỗi. `cargo test -p zalo-client --offline --lib` sau khi gỡ helper HTTP: 63 test, 0 lỗi. Số này là của lượt gỡ đồng bộ. Crate `zalo-sync2` đã bị xóa hết ở lượt gỡ sao lưu, nên không chạy lại test của crate đó.

## Log của lần chạy sai

Log worker cũ, 39 dòng, không chứa khóa hay id tài khoản. Các mốc:

1. «phiên thủ công, tempKey rỗng». TempKey rỗng của lần bấm tay là đúng với Electron.
2. Sáu trang danh sách: nhận 100, 100, 100, 100, 100, rồi 39. Hội thoại mới mỗi trang: 100, 99, 99, 99, 99, 38. Tổng 534. Hội thoại biên bị lặp giữa các trang.
3. Mỗi trang danh sách là một khung `queries=1`. Id yêu cầu là `req_1`, `req_3`, `req_5`, `req_7`, `req_9`, `req_11`. Số lẻ vì sau mỗi trang worker còn gửi thêm một lệnh đóng phiên.
4. Sau danh sách: «kéo tin của 534 hội thoại, 18 yêu cầu», rồi «kéo tin lô 1/18, hội thoại=30», «gửi req_13 … queries=30».
5. Socket đóng: peer đóng TLS mà không gửi `close_notify`. Không có dòng «đủ lô» cho pha tin. Không có số tin được ghi.
6. Trước khi chia trang, một khung nhét cả 534 partition bị server đóng với mã WebSocket `1009` (`MESSAGE_TOO_BIG`).

Dòng giao diện «Điện thoại đã đồng ý, đang chờ dữ liệu…» là câu của `Progress::Confirmed` trong worker Rust. Electron không có câu đó. Worker in câu này rồi đứng trong `drive()`, nên màn hình không đổi khi lô tin không về. Nhánh «mất mạng» chỉ chạy khi chưa confirm, nên sau confirm một lần rớt TLS không đổi banner.

## Chỗ lệch với Electron

Nguồn là `giao-dien/02-js/compact-app/modules/` và `giao-dien/01-goc/pc-dist/shared-worker.d6af5465eb38c618bfe0.js`. Chi tiết từng hàm nằm ở file hướng triển khai.

1. **Đóng phiên giữa các trang danh sách.** Electron giữ database hội thoại mở suốt vòng trang. Hết mọi trang và thành công thì gọi `closeConversationRemoteDatabase` **một lần**, lý do dispose `SyncSuccess` = 1. Rust gửi lệnh `591` sau từng trang. Log `req` lẻ là dấu vết của việc đó.

2. **`start()` trả về khi socket ACK, không đợi lô cuối.** Session manager tạo phiên sau, tempKey của phiên sau là chuỗi rỗng, và không `release()` phiên trước. Các nhóm prepare chồng lên nhau sau ACK. Rust đợi `isLast` rồi hủy phiên rồi mới gửi nhóm sau.

3. **Pha tin không phải 30 partition GetAll với limit vô hạn.** Electron có hai bước:
   - Prepare: nhóm query, mỗi nhóm `getQueryCount() < 30`, mỗi khoảng thiếu bị ép limit `2147483647`, kiểu `PrepareAll`.
   - Đọc tin: **một hội thoại một query** `GetAll`, limit là `batch_size` từ remote config. Fallback đóng gói của khóa đó là `2000`, không phải `2147483647`, và không phải 30 partition trong một khung.
   Rust gửi khung `queries=30`, mỗi partition limit `2147483647`, rồi chặn. Peer đóng TLS ở khung đầu.

4. **Cỡ trang danh sách.** Fallback đóng gói của Electron (`sync_2_pc`) là `conversations_batch_size = 1000`. Trang đầu còn bị thay bằng `manual_sync.sync_last_chat.max_number_of_conv` khi số đó dương; fallback của khóa này là `10`. Rust tự gắn `100`. Server đã nhận các trang 100 trong log, nên 100 không phải lỗi giao thức theo nghĩa «server từ chối», nhưng đó không phải số của Electron.

5. **Hai kiểu trừ biên, Rust trộn một kiểu.** Trong một lần `executeGetAllQuery`, trang tiếp theo của **cùng một query** đặt `range.to = index(dòng cuối) - 1`. Vòng trang hội thoại phía renderer đặt `toTs = lastTs` của hội thoại cuối, **không** trừ 1. Vòng tin của một hội thoại cũng đặt `toTs = lastTs`, không trừ 1, và `isCompleted = false` để hỏi lại hội thoại đó.

6. **Phần trăm.** Electron chỉ tính `floor(completedCount / totalCount * 100)` khi một khoảng thiếu thực sự `isCompleted`. `totalCount` là số khoảng thiếu lúc bắt đầu pha tin còn lại. Không phải số trang, không phải đồng hồ. Rust không có lô tin nên không có phần trăm thật, và cũng không được bịa.

7. **Timeout confirm.** Fallback Sync 2 cho đồng bộ tay là `manual_sync.awaiting_mobile_timeout_ms = 600000` (10 phút). `cross_setting.cfTimeout = 180` là cấu hình đường cũ, đơn vị giây, không phải timeout của máy Sync 2. Rust dùng 180 giây cho cửa sổ confirm của Sync 2.

8. **TempKey tự động.** `transfer_after_login` chỉ được nhận khi `act_type` là `transfer_msg` (Sync 1), `temp_key` khác rỗng, `imei` trùng client id của máy, và `now - ts <= 600000`. Sự kiện đó mới gọi `startAutoSyncFull`. Đồng bộ tay (`startManualSync`) không gửi `tempKey`. Rust đã đúng phần tempKey rỗng của lần bấm tay; không được lấy sự kiện `transfer_sync2` để điền tempKey.

9. **HTTP `/api/transfer-sync-v2/request-sync` (lệnh catalog `12888`)** có hàm bọc trong `compact3178.js` và một dòng catalog trong `compact0598.js`. Không có compact module nào gọi hàm HTTP đó. Đường đang chạy là socket `590` qua `Kte.requestSyncMessage`. `pull_mobile_msg` và `cancel_pull_mobile_msg` là đường cũ, không được dựng lại khi Sync 2 còn sống.

10. **`compact2331.js` chỉ là logger báo cáo.** Không phải giao thức.

## Cái giữ lại, và vì sao không phải mã đồng bộ

Rà `*.rs` sau khi gỡ: không còn `phone_sync`, `CMD_REQUEST_SYNC`, `pull_mobile`, `request_transfer_sync`, `sync-temp`, `SyncPrompt`, `StartPhoneSync`, `pendingSyncPrompt`, `confirm_needs_sync`.

Còn những thứ sau. Chúng không phải máy đồng bộ điện thoại sang PC:

| Chỗ còn | Vì sao giữ |
| --- | --- |
| Crate `zalo-sync2` | Đã bị xóa hết ở lượt gỡ sao lưu PC sang điện thoại. Không dựng lại crate này cho đồng bộ, cũng không dựng lại cho sao lưu. |
| `assets/wasm/libzproto_backup_wasm_bg.042497d03f0f504b6b1444e25a7db0d9.wasm` | File wasm sao lưu nằm trên đĩa. Không file `.rs` nào nạp nó sau khi máy sao lưu bị gỡ. |
| `assets/wasm/libzproto_wasm_bg.ecc0555ba9d0f8867a4699ce943ec745.wasm` | Đúng file wasm Sync 2 của Electron. Không file `.rs` nào còn gọi nó. Giữ bytes để lần viết lại đồng bộ dùng đúng module. |
| `Frame` trong `zalo-client` `listener.rs` | Khung socket dùng chung của client: version `u8`, cmd `u16` little-endian, subCmd `u8`, rồi JSON. Không còn nhánh lệnh sao lưu. |
| Bảng chuỗi `strings_vi.rs` / `strings_en.rs` | Bản dịch Electron, gồm câu «Đồng bộ tin nhắn» và «Sao lưu tin nhắn». Không phải logic. Khóa xuất/nhập vẫn dùng vài câu của catalog. Không xóa catalog. |
| `docs/doi-chieu-electron-rust.md` | File đối chiếu cũ, khoảng 18 nghìn dòng, vẫn mô tả `590` / HTTP request-sync theo số dòng cũ. **Không còn đúng cho phần đồng bộ tin nhắn.** Không viết lại cả file trong lượt này. Khi đọc đối chiếu, bỏ qua mục đồng bộ điện thoại của file đó và dùng hai file trong `docs/` vừa thêm. |
| Số `591` trong `fonts.rs` | Id glyph font, không phải lệnh socket. |

`docs/doi-chieu-electron-rust.md` và hai bảng chuỗi là chỗ còn chữ «đồng bộ tin nhắn» trong cây nguồn. Đó là tài liệu cũ và catalog dịch, không phải worker.

## Việc cố ý chưa làm

Chưa viết lại máy đồng bộ. Chưa bấm Đồng bộ hay Hủy trên tài khoản đang sống. Process `zalo-gpui` đang mở, nếu còn là binary cũ, vẫn chứa worker đã gỡ ở mã nguồn cho đến khi được thay bằng binary build sau báo cáo này. Tắt process đó là bỏ hộp thoại đang chờ trên điện thoại; không bấm Hủy trong app để «dọn».
