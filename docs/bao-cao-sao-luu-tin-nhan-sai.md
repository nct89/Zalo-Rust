# Báo cáo: mã sao lưu tin nhắn trên Rust đã lệch Electron

Ngày đối chiếu: 2026-11-21.

Phạm vi của bản này là **sao lưu tin nhắn từ PC sang điện thoại**. Ba việc không được trộn:

- Đồng bộ tin nhắn từ điện thoại sang PC (socket `590`) đã bị gỡ trước. Không dựng lại. Xem `docs/bao-cao-dong-bo-tin-nhan-sai.md`.
- Sao lưu tin nhắn PC sang điện thoại: socket `630`–`634` và `POST /api/backuppc2mb/upload`. Máy này đã bị gỡ. Tài liệu này ghi hướng sai.
- Xuất / Nhập dữ liệu: file zip trên máy này. Phần này còn, nằm ở `pc_export.rs` (`TransferUi`).

`docs/huong-trien-khai-sao-luu-tin-nhan.md` là hướng viết lại từ nguồn Electron vừa đọc lại. Lượt này không viết mã mới.

Nguồn Electron đã đọc lại, không lấy từ topic cũ:

- `giao-dien/02-js/compact-app/modules/compact2101.js` — số lệnh.
- `compact3857.js` — năm phương thức socket và `addCommonParams`.
- `compact3197.js` object `b` — default đóng gói `backup_msg`.
- `compact1115.js` — `BackupConfigService`.
- `compact3178.js` — `uploadBackupChunk`.
- `compact0347.js` export `q` — `SEND_CHUNK: 1`.
- `compact3432.js` module `ie40` — timeout upload.
- `compact0508.js` — popup, rộng 400, trễ loading 1 giây.
- `02-js/lazy/main-startup.fe4e52efd649575b333b.js` — spam, phiên, pipeline, upload, hành động người dùng.
- `01-goc/pc-dist/shared-worker.d6af5465eb38c618bfe0.js` — dựng scope, lọc id, protobuf, nén, mã hóa, ghi block.
- `02-js/lazy/default-main-startup-shared-worker.7c65612c9b54921b5cc6.js` — task `GET_BACKUP_NOISED_IDS` và helper muối 32 byte.

`compact0616.js` và `compact1480.js` có hàm tên `isFeatureEnabled` nhưng không phải cờ `backup_msg.enabled`.

## Việc đã gỡ

Đã xóa máy sao lưu và chỗ bật nó:

- `zalo-linux/crates/zalo-gpui/src/backup_msg.rs`.
- Crate `core/zalo-sync2` (protobuf text, host wasm backup, `wasmi`). Workspace và `zalo-gpui` không còn phụ thuộc crate này.
- `ZaloApi::upload_backup_chunk` trong `core/zalo-client/src/api.rs`.
- Thẻ Cài đặt `sync-backup` và nhánh `SAction::ExportBackup`.
- `RtEvent::BackupReply`, kênh `SYNC_OUT`, và chỗ listener chờ lệnh `630`–`634`.
- Banner sao lưu trên khung danh sách.

Sheet Xuất/Nhập trước nằm chung file sao lưu. Sheet đó đã chuyển sang `pc_export.rs`. Bấm thẻ «Xuất dữ liệu/ Nhập dữ liệu» vẫn mở sheet zip. Không còn đường nào gửi `631`, `630`, upload, hay `633`.

`cargo test -p zalo-gpui -p zalo-client --offline --lib` sau khi gỡ: `zalo-client` 63 test, 0 lỗi; `zalo-gpui` 185 test qua, 0 lỗi, 4 test bỏ qua. `cargo build -p zalo-gpui --offline --bin zalo-gpui` xong, exit 0. Warning còn lại là cảnh báo sẵn của crate, không chặn build.

Không có process `zalo-gpui` đang chạy sau bước này, nên không mở lại cửa sổ.

## Chỗ lệch của mã đã gỡ

Các điểm dưới đây là hành vi của máy Rust đã xóa, đối chiếu với Electron vừa đọc. Số lệnh, cỡ chunk `3145728`, cửa sổ tái dùng `1740000` ms, TTL đóng gói `259200000` ms, spam 6 lần / 60 giây / khóa 120 giây, cooldown 600 giây, và retry socket 3 lần cách 10 giây khi timeout, là những hằng số trùng bản đóng gói. Phần lệch nằm ở cách dùng chúng.

1. **Thẻ luôn hiện.** Electron: `backup_msg.enabled` đóng gói là `false`. Remote config có thể bật. Khi tắt, `onServicesReady` giấu UI sao lưu và xóa trạng thái gián đoạn. Bấm lúc cờ tắt chỉ ghi QoS `feature_disabled`, không mở popup. Rust hiện thẻ không qua cờ này.

2. **Gói tin không phải dump text của `store.db`.** Electron dựng file trong shared worker. Trang hội thoại 500, tối đa 200 trang. Mỗi scope là `{from: 0, to: Number.MAX_SAFE_INTEGER, partition: userId, limit: 100000}`. Rồi `dropUnnoisableScopes` gọi task `GET_BACKUP_NOISED_IDS`. Nếu map không giải được id nào thì giữ mọi scope. Nếu giải được một phần thì bỏ partition không có trong map. Mỗi block: đọc, `transform2CrossData`, protobuf `SyncChunk`, nén zstd, `encryptMessage` qua wasm backup, rồi ghi. Rust dựng hàng text, bỏ tin rỗng / `sendBubbleMessage` / thiếu người gửi, đổi tiền tố `g_` thành `g`, và không có bước `GET_BACKUP_NOISED_IDS`.

3. **Nonce rỗng không phải đường mặc định.** `encryption_enabled` đóng gói là `true`. Renderer chỉ đưa `serverNonce: ""` khi mã hóa tắt. Mã hóa bật mà thiếu nonce hoặc muối thì ném lỗi trước worker. Worker từ chối thiếu `clientSalt`, và bắt `serverNonce` là chuỗi (`""` mới là raw). Rust coi nonce rỗng là ghép khung không qua wasm.

4. **Khung file.** Rust gắn mỗi block bằng độ dài `u32` little-endian rồi nối vào một file. Electron ghi từng block qua file manager, tên theo chỉ số block, và `isCurrentBlockFull` của lớp đó luôn trả `false`. Đường upload lấy file bằng `getSync2BackupFilePath`. Cách các block thành một file tải lên chưa được đọc hết trong lượt này. Không được lấy layout `u32` của Rust làm giao thức.

5. **Lệnh `633`.** Electron chỉ gửi từ «Dùng bản sao lưu này» và từ banner thành công «tiếp tục trên điện thoại». Banner hiện trước, rồi `My()` gọi `633` và nuốt lỗi (chỉ log). Rust đợi `633` thành công rồi mới hiện trạng thái sau upload.

6. **`backup_done`.** Sau upload thành công, đường đang chạy gọi `showSuccessBanner` rồi `armBackupDoneMonitor`. Push `backup_done` khớp `bkid` thì ghi QoS. Hết `backup_done_timeout_seconds` (180) thì monitor timeout, không đổi banner thành công. Hàm `armBackupDoneWait` (giữ 99% tới khi có xác nhận) có trong bundle và không có chỗ gọi. Rust không có monitor này, cũng không có tự chạy lại phiên bị cắt.

7. **Phần trăm.** Worker: `min(99, floor(số scope đã xong / tổng scope * 100))`, hết gói thì báo 100. UI đóng gói là `floor(phần trăm worker * backup_progress_weight / 100)`, weight đóng gói 90. Upload là `floor(weight + phần trăm chunk * (100 - weight) / 100)`. Rust dùng các mốc cố định 10 / 40 / 90.

8. **Cooldown và spam là hai UI.** Cooldown (`now - lần thành công < 600 giây`) hiện toast, không ghi thêm một request. Spam trên popup bản đã có là toast. Spam trên popup giới thiệu là popup khóa. Rust gộp một popup spam.

9. **`632`.** Mã `1001` và mọi mã khác khác 0, hoặc thiếu `data`, đều thành trạng thái NONE rồi popup giới thiệu. QoS coi `1001` là thành công, mã khác là lỗi. Exception bên trong `getBackupStatus` cũng bị nuốt thành NONE. Popup mạng lúc mở thẻ đến từ guard `network_disconnected` trước khi gọi `632`, không phải từ exception của `632`. Rust cũng mở intro khi không có metadata, nhưng không tách QoS và không có popup lỗi chung của nhánh controller.

10. **Tái dùng phiên.** Trước engine: giữ khi có đủ `bkid`, `serverNonce`, `clientSalt`, và (`bkidCreatedAt` chưa đặt hoặc tuổi ≤ `1740000` ms). Không đủ thì gọi lại `631`. Muối là 32 byte ngẫu nhiên, base64, sinh local, không lấy từ server. Trước `630`: chỉ bỏ qua `630` khi đã có `bkid` và tuổi đã biết và tuổi ≤ cửa sổ. Tuổi chưa biết thì không bỏ qua. Quá cửa sổ thì xóa khóa và ném «CMD 630 upload window expired». Mã `212` của `630` xóa `bkid`, nonce, muối và con trỏ chunk. Rust chỉ so một mốc thời gian trong file `backup-session.json`.

11. **Retry upload.** Ngân sách là `SEND_CHUNK + 1` = 2, vòng lặp thành 3 lần. Timeout đợi 1 giây. Mất mạng đợi nối lại tối đa 50 giây, hỏi mỗi 5 giây. Mã 112 và 220 đợi 10 giây. Lỗi khác không thử lại. Hàm `uploadBackupChunk` đặt timeout HTTP `60000` ms. Biến timeout tăng dần (bước 60 giây, trần 120 giây) được log và không thấy truyền vào hàm upload. Rust không dùng ngân sách này.

Hằng `ERR_NONE = 1001` trong module Rust không phải tên lỗi của Electron. Electron dùng số `1001` như một `error_code` của `632` nghĩa là không có bản sao lưu.

## Cái giữ lại

Rà `*.rs` sau khi gỡ: không còn `backup_msg`, `upload_backup`, `BackupReply`, `zalo-sync2`, `backuppc2mb`, `ExportBackup`, `sync-backup`.

| Chỗ còn | Vì sao giữ |
| --- | --- |
| `pc_export.rs` (`TransferUi`, `TransferPopup`) | Zip Xuất/Nhập của chính client này. Manifest `zalo-rust-pc-export` phiên bản 2. Không gửi socket sao lưu. |
| `build_chat_backup_json` trong `settings.rs` | JSON chỉ cho unit test (`zalo-gpui-backup-v1`). Comment trong file ghi rõ đây không phải sao lưu PC sang điện thoại. Không có chỗ gọi ngoài test. |
| Bảng chuỗi `strings_vi.rs` / `strings_en.rs` | Catalog Electron. Sheet xuất/nhập còn dùng vài khóa `STR_BACKUP_MSG_*`. Không xóa catalog. Câu «đang sao lưu tin nhắn» không còn đường chạy tới. |
| Hai file wasm trong `assets/wasm/` | Bytes của Electron. Không file `.rs` nào nạp chúng. |
| `Frame` trong `listener.rs` | Khung socket dùng chung của tin nhắn và các lệnh khác. |
| Hộp thoại `.zdb4` (`SAction::BackupDecrypt`) | Đường giải mã file local, không phải thẻ `630`–`634`. |
| `docs/doi-chieu-electron-rust.md` | File đối chiếu cũ, khoảng 18 nghìn dòng. Mục sao lưu và mục đồng bộ của file đó đã cũ. Không viết lại cả file trong lượt này. |
| Số `591` trong `fonts.rs` | Id glyph, không phải lệnh socket. |

`Cargo.lock` được cargo viết lại khi test. Crate `zalo-sync2` không còn trong workspace.

## Việc cố ý chưa làm

Chưa viết lại máy sao lưu. Chưa bấm «Tạo mới», «Dùng bản sao lưu này», Xuất, hay Nhập trên tài khoản đang sống. Handler main-process của task `GET_BACKUP_NOISED_IDS` và hàm nối file của `getSync2BackupFilePath` chưa được đọc hết. Hai chỗ đó nằm trong file hướng triển khai như việc phải đọc trước khi viết mã.
