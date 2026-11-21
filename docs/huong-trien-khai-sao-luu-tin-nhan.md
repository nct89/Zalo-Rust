# Hướng triển khai lại sao lưu tin nhắn từ nguồn Electron

Ngày đọc nguồn: 2026-11-21.

**Lượt này không viết máy sao lưu.** File này là hướng cho lần sau, khi được yêu cầu. Đừng mở thẻ, đừng gửi `631` / `630` / `633`, đừng upload, và đừng bấm «Tạo mới» hay «Dùng bản sao lưu này» trên tài khoản đang sống.

Sao lưu ở đây là PC sang điện thoại. Đồng bộ điện thoại sang PC là file `docs/huong-trien-khai-dong-bo-tin-nhan.md`. Xuất/Nhập zip là `pc_export.rs`. Ba việc đó không dùng chung một giao thức.

`docs/doi-chieu-electron-rust.md` đã cũ ở mục này. Đọc file này và nguồn Electron, không đọc bảng đối chiếu cũ.

## Nguồn

Đọc lại các file sau trước khi viết mã. Đường dẫn gốc là `giao-dien/`:

- `02-js/compact-app/modules/compact2101.js` — số lệnh.
- `02-js/compact-app/modules/compact3857.js` — lớp socket `m`.
- `02-js/compact-app/modules/compact3197.js` — object default `b`.
- `02-js/compact-app/modules/compact1115.js` — service cấu hình.
- `02-js/compact-app/modules/compact3178.js` — `uploadBackupChunk`, catalog `13538`.
- `02-js/compact-app/modules/compact0347.js` — module `2ZOf`, export `q`.
- `02-js/compact-app/modules/compact3432.js` — module `ie40`.
- `02-js/compact-app/modules/compact0508.js` — popup.
- `02-js/lazy/main-startup.fe4e52efd649575b333b.js` — controller, khoảng dòng 37290–41940.
- `01-goc/pc-dist/shared-worker.d6af5465eb38c618bfe0.js` — worker dựng file.
- `02-js/lazy/default-main-startup-shared-worker.7c65612c9b54921b5cc6.js` — module `qZF7` và `qg/u`.

`compact0616.js` và `compact1480.js` không phải cờ sao lưu.

Trong worker và module muối có các bảng byte cố định. Không chép các số đó vào tài liệu hay vào Rust. Khi cần khóa của wasm, đọc lại module lúc viết mã.

## Lệnh và thân JSON

`compact2101.js`:

| Tên | Số |
| --- | --- |
| `CREATE_SESSION` | 631 |
| `INIT` | 630 |
| `GET_METADATA` | 632 |
| `SIGNAL_RESTORE` | 633 |
| `GET_CONFIGS` | 634 |

Mỗi method trong `compact3857.js` đặt `subCmd: 0` và một `reqId` trong `data`. `addCommonParams` ghi đè `data.reqId` bằng `getReqId(cmd)`. ReqId viết sẵn trong method bị bỏ.

Thân sau khi bọc:

- `631`, `632`, `633`, `634`: `{ cmd, subCmd: 0, data: { reqId } }`.
- `630`: `{ cmd, subCmd: 0, data: { reqId, data: metadata } }`. Object metadata đã được gộp `bkid` và `client_salt` trước khi đưa vào `data.data`.

Timeout ở chỗ gọi là `10000` ms. `requestAsyncV2` nhận số đó từ tham số mặc định của từng method.

Khung nhị phân bên ngoài là khung socket đang dùng của client (`Frame` trong `listener.rs`: version `u8`, cmd `u16` little-endian, subCmd `u8`, rồi JSON). Đừng thiết kế một phong bì mới. Đọc lại `requestAsyncV2` nếu phong bì Electron khác khung này trước khi đổi `Frame`.

## Mở thẻ

Popup host là `De` trong `compact0508.js`. Khi `params.step` là Hidden, effect gọi `onUserRequestBackup()`.

`onUserRequestBackup` chạy guard `feature_disabled` rồi `network_disconnected`:

- Cờ tắt: chỉ QoS `GuardFeatureDisabled`. Không popup. `onServicesReady` cũng giấu UI sao lưu và xóa interrupt state khi cờ tắt.
- Mạng `DISCONNECT`: popup mạng.
- Cả hai qua: `showLoading()`, rồi `yy()`, rồi `getBackupStatus()`.

`yy` gọi `634` trừ khi `isServerConfigsFresh()`. TTL cache đóng gói `backup_configs_cache_ttl_ms` là `-1`. `isServerConfigsFresh` trả false khi TTL nhỏ hơn 0. Mỗi lần mở thẻ đều gửi `634`.

`634` lỗi, data rỗng, thiếu `configs`, hoặc ném exception: trả `null`, người gọi giữ cấu hình đóng gói. Chỉ chép `retention_time` khi là số lớn hơn 0. `getBackupTtlMs` dùng số server đó, không có thì dùng `backup_ttl_ms`.

Popup rộng 400. Loading bị trễ `1000` ms: trước đó modal trả `null`. GenericError gắn nút thử lại vào handler đóng. NetworkError mới gọi `onUserRetryInit`.

Chưa lần được renderer của hàng Cài đặt có ẩn hàng khi `enabled` là false hay không. Guard lúc bấm đã rõ. Lần viết lại phải đọc chỗ gắn menu trước khi quyết định hàng có hiện không.

## Kết quả 632

`getBackupStatus`:

- `error_code` là `1001`: trạng thái NONE, QoS thành công.
- Thiếu data, hoặc `error_code` khác 0: trạng thái NONE, QoS lỗi `InitServerError`.
- Exception trong method: nuốt, trạng thái NONE, QoS lỗi. Method này không ném ra ngoài.
- `error_code` 0 và có `data.metadata`: EXISTING. Đọc `pc_device_name`, `size`, `created_time`, `expired_time`, `bkid`. `expired_time` thiếu thì TTL là `now + getBackupTtlMs()`.

Controller `Cy`: NONE mở popup giới thiệu. EXISTING qua `Iy`. Trạng thái khác mới là popup lỗi chung. Vì method nuốt exception, nhánh `Ey` (mạng nếu `code === "NO_NETWORK"` hoặc `isNetworkError`, còn lại lỗi chung) không chạy cho lỗi socket của `632` hiện tại.

`Iy`: nếu `createdTime > 0` và `now > createdTime + getBackupTtlMs()` thì đổi sang popup giới thiệu. Ngược lại hiện popup bản đã có: tên máy, kích thước, thời điểm, hạn.

## Cấu hình đóng gói

Object `b` trong `compact3197.js`, tên remote `backup_msg`, schema trong `compact1115.js`. Remote có thể đè.

| Khóa | Default | Cách dùng đã đọc |
| --- | --- | --- |
| `enabled` | false | Cờ thẻ. |
| `max_request` | 6 | Spam. |
| `fixed_time` | 60 giây | Cửa sổ đếm request. |
| `restrict_time` | 120 giây | Thời gian khóa. |
| `create_backup_waiting_time` | 600 giây | Nhân 1000 thành cooldown. |
| `max_file_size` | 0 | Chỉ chặn khi lớn hơn 0. |
| `chunk_size` | 3145728 | Cỡ chunk upload. |
| `backup_progress_weight` | 90 | Phần trăm UI của pha đóng gói. |
| `after_success_auto_hide_banner_seconds` | 300 | Banner thành công. |
| `after_fail_auto_hide_banner_seconds` | 300 | Banner lỗi. |
| `after_interrupt_auto_hide_banner_seconds` | 300 | Banner gián đoạn. |
| `sleep_resume_banner_hold_seconds` | 3 | Giữ banner sau sleep. |
| `backup_done_timeout_seconds` | 180 | Monitor `backup_done`. |
| `auto_resume_window_seconds` | 300 | Cửa sổ tự chạy lại. |
| `auto_resume_max_attempts` | 3 | Số lần tự chạy lại. |
| `auto_resume_backoff_ms` | 2000, 8000, 30000 | Lùi thời gian. |
| `backup_ttl_ms` | 259200000 | 3 ngày, khi server không đưa retention. |
| `encryption_enabled` | true | Mặc định mã hóa. |
| `backup_configs_cache_ttl_ms` | -1 | Cache `634` luôn cũ. |
| `crypto_version` | 1 | Ghi vào metadata. |
| `db_format_version` | 1 | Ghi vào metadata. |

`-1` ở các khóa auto-hide được `secondsToMsWithSentinel` giữ nguyên `-1`, không nhân 1000.

## Spam và cooldown

`recordRequest` giữ mốc trong `fixed_time` giây. Sau khi đẩy mốc mới, nếu số mốc lớn hơn `max_request` thì xóa danh sách và khóa `restrict_time` giây. Lần thứ 7 trong 60 giây khóa 120 giây.

Cooldown: `waitingMs > 0`, đã có lần thành công, và `now - lastSuccess < waitingMs`.

Trên nút tạo mới (`onUserConfirmNewBackup`):

1. Đang chạy thì dừng, QoS concurrent.
2. Đang cooldown thì toast, không `recordRequest`.
3. `enforceSpamLimit`: đang khóa thì toast nếu popup là bản đã có, popup khóa nếu là popup giới thiệu. Không ghi request.
4. Việc nặng khác (`coordinator.isExternalBusy`) thì log và dừng, không ghi request. `checkOne("external_busy")` lại luôn trả ok. Đường confirm dùng `isExternalBusy`, không dùng check đó.
5. Qua các cửa trên thì `recordRequest`, rồi kiểm mạng, giấu popup, `startFreshBackup`.

## 631, muối, 630

`createBackupSession` thử tối đa 3 lần, cách `10000` ms, chỉ khi `isRetryableInitError`: mã `-3`, `-2`, `ERR_CONNECTION_TIMED_OUT`, `ECONNABORTED`, `ETIMEDOUT`, `ERR_NO_NETWORK`, `NO_NETWORK`, `ENETUNREACH`, hoặc message chứa `timeout`, `timed out`, `network`. `error_code` ngoài khác 0 hoặc `data.err` khác 0 thì ném ngay, không thử lại. Thiếu `bkid` hoặc `server_nonce` thì ném. Không có đường identity khi thiếu nonce.

`initBackupSession` cùng luật retry. Response phải có `bkid`. Thân gửi là metadata cộng `bkid` và `client_salt`.

Muối client là `qg/u` export `b`: mặc định 32 byte `crypto.getRandomValues`, rồi base64. Sinh local sau `631`. Không lấy từ server.

Trước engine, hàm tái dùng (`oy = 1740000`):

- Giữ phiên khi có `bkid` và `serverNonce` và `clientSalt`, và (`bkidCreatedAt` chưa gán hoặc tuổi ≤ `oy`).
- Không đủ thì gọi lại `631`, ghi nonce server, sinh muối mới, xóa `bkidCreatedAt` và `lastUploadedChunkId`.

Trước `630`:

- Có `bkid` và tuổi đã biết và tuổi ≤ `oy`: bỏ qua `630`, dùng `bkid` đó.
- Tuổi đã biết và lớn hơn `oy`: xóa `bkid`, nonce, muối, mốc, con trỏ chunk, ném «CMD 630 upload window expired». Lần thử sau bắt đầu từ `631`. Không tạo phiên mới ngay trong hàm này.
- `bkidCreatedAt` chưa gán: không bỏ qua, gửi `630`.

Mã `212` trên lỗi `630` xóa `bkid`, nonce, muối, mốc và con trỏ chunk.

Mã hóa bật mà thiếu muối hoặc nonce: renderer ném trước worker («CMD 631 must run before engine» / nonce missing). Worker từ chối thiếu `clientSalt`. `serverNonce` phải là chuỗi. Chuỗi rỗng chỉ khi mã hóa tắt, nghĩa là raw.

`myPcNoisedUserId` thiếu thì worker chỉ log: `senderId` 0 không noise được. Worker vẫn chạy tiếp. Lần viết lại phải có id này trước khi đóng gói, sau khi đọc chỗ renderer điền field.

## Worker dựng file

`buildMessageScopes`:

- Lấy hội thoại theo trang 500, tối đa 200 trang.
- Mỗi hội thoại một scope `{ from: 0, to: Number.MAX_SAFE_INTEGER, partition: conversation.userId, limit: 100000 }`. Hằng `wa` trong worker là `1e5`.

`dropUnnoisableScopes` gọi `pcNoised2gNoisedMulti`. Lớp đó chạy task type chuỗi `GET_BACKUP_NOISED_IDS`, params `{ ids }`. Chuỗi này có thật trong module `qZF7`. Nếu không id nào được giải, giữ hết scope và đánh dấu lookup outage. Nếu có id được giải, bỏ scope có partition không nằm trong map.

Handler main-process nhận task `GET_BACKUP_NOISED_IDS` chưa được đọc trong lượt này. Không bịa đường HTTP. Đọc handler đó trước khi viết bước noise.

Mỗi block, theo thứ tự task của worker: `readData`, `transform2CrossData`, `encodeData`, `compressData`, `encryptData`, rồi `write`.

`serializeMessage` dựng `proto.Sync2.Message.SyncChunk`: platform, rồi partition có id và danh sách message. Các setter đã thấy trên message: `senderId`, `clientId`, `msgStatus`, `msgType`, `timestamp`, `globalId`, `content`. `meta` có attach (`type`, `extInfo`, `action`, `params`, `title`, `href`, `thumb`, `description`, `catId`, `id`, `childNumber`, `remains`), mention (`uid`, `pos`, `len`, `type`), reference (`type`, `data`), quote (`attach`, `ownerId`, `ts`, `fromD`, `cliMsgId`, `globalMsgId`, `msg`, `cliMsgType`, `ttl`, `partialQuote`). Cùng hàm còn setter `Size`, `Color`, `Type`, `SubType`, `Ext`, `Property`, `Ttl` trước `setMeta`. Đọc nốt đoạn đó lúc viết mã để không đoán nhầm field nào nằm trong message và field nào nằm trong property.

Nén là zstd stream của worker. Mã hóa là wasm `libs/libzproto_backup_wasm_bg.042497d03f0f504b6b1444e25a7db0d9.wasm`, `proto.start(backupId, base64(nonce), base64(salt))`, rồi `encryptMessage`. File wasm cùng tên đang nằm trong `assets/wasm/` và chưa có code Rust nạp nó.

`globalThis.doBackupConv` và `globalThis.doBackupMessage` là hook debug. Không phải đường đang chạy. Nonce và id mẫu trong hai hook đó không phải giao thức.

Ghi block: `write` gọi file manager với path `getPath(backupDir, currentIdx)`. `isCurrentBlockFull` của lớp này trả `false`. Cuối gói, đường upload gọi `getSync2BackupFilePath(path, "0")`. Chưa đọc `Xn.a.write` và `getSync2BackupFilePath` đủ để mô tả file trên đĩa có phải nối bằng độ dài `u32` hay không. Rust cũ làm kiểu đó. Đừng mang layout đó sang bản mới cho đến khi đọc hai hàm này.

SHA-256 là của một file upload (`readBackupFileWithChecksum` / `Gb`), hex thường, không phải hash từng chunk.

## Metadata trước 630

Khóa:

- `pc_device_name`: hostname.
- `min_msg_ts`: 0.
- `size`: kích thước file.
- `original_size`: `originalSize` của worker.
- `msg_count`: `itemCount`.
- `thread_count`: `scopeCount`.
- `crypto_version`, `db_format_version`.
- `checksum`: SHA-256 của file.

Integrity trước `630` fail khi `msg_count`, `thread_count`, hoặc `size` không khớp worker, hoặc size ≤ 0, hoặc checksum rỗng. `original_size` ≤ 0 chỉ log.

`scopeCount` 0: banner lỗi `UserSystem`, reset phiên. Không phải popup «không có dữ liệu». `itemCount` 0 trong khi còn scope: lỗi integrity. `filePath` rỗng: popup lỗi chung.

## Phần trăm

Trong worker, `f` tăng khi block là FULL hoặc LAST. `y` là tổng scope. Giữa chừng: `y > 0 ? min(99, floor(f / y * 100)) : 0`. Hết gói thì callback 100.

UI đóng gói: `floor(phần trăm worker * weight / 100)`.

UI upload: `floor(weight + phần trăm chunk * (100 - weight) / 100)`. Phần trăm chunk là `floor(chunk hiện tại / tổng chunk * 100)`.

Resume upload bắt đầu ở `lastUploadedChunkId + 1`.

## Upload

`uploadBackupChunk` trong `compact3178.js`:

`getBackupPcMobileDomain() + "/api/backuppc2mb/upload?" + common params`.

Catalog lệnh `13538`, domain `BACKUP_PC_MOBILE`.

`FormData`:

- `param` = `encodeAES(JSON.stringify({ bkid, batch_data }))`.
- `batch_data` là chuỗi JSON của `{ last: 0 hoặc 1, chunk_id: chỉ số 1-based, total_chunk, size: fileSize }`.
- `file_data` là `File` tên `backup_chunk`, type `application/octet-stream`.

Timeout trong `uploadBackupChunk` là `60000` ms, `withCredentials: true`.

Vòng chunk tính thêm một timeout tăng dần: điểm đầu `upload_file_timeout_start_time` hoặc `60000`, mỗi lần timeout cộng `60000`, trần `120000`. Giá trị này được log trong vòng `kb`. Lời gọi `uploadBackupChunk` chỉ nhận `FormData`. Chưa thấy timeout tăng dần được truyền vào HTTP.

Ngân sách `Mb = export q.SEND_CHUNK + 1`. Export `q` là object `f` với `SEND_CHUNK: 1`, nên `Mb` là 2. Vòng kiểm `n <= 0` rồi mới giảm `n`, nên có 3 lần thử.

- Timeout (`ERR_CONNECTION_TIMED_OUT`, `ECONNABORTED`): đợi 1 giây, tăng biến timeout.
- Mạng (`ERR_NO_NETWORK`, `NO_NETWORK_ERR`, `NO_NETWORK`, hoặc `isNetworkError`): đợi nối lại tối đa `50000` ms, hỏi mỗi `5000` ms.
- Mã server `112` và `220`, hoặc code `in2_112`, hoặc message `ERR_REQUEST_ABORT`: đợi `10000` ms.
- Lỗi khác: không thử lại.

`max_file_size` lớn hơn 0 và file lớn hơn mức đó thì dừng trước upload. Default 0 nên bước này bỏ qua.

Sau upload, `hy` kiểm:

- `ceil(fileSize / chunk_size)` bằng `totalChunks`.
- `lastUploadedChunkId` bằng `totalChunks`.
- Nếu metadata có checksum thì checksum vừa tính phải trùng checksum metadata.

## Sau upload và lệnh 633

Đường thành công đang chạy: đánh dấu hoàn tất, xóa interrupt state, `showSuccessBanner(fileSize)`, rồi `armBackupDoneMonitor(bkid)`.

`633` không nằm trong đường upload. Chỉ có:

- `onUserConfirmRestoreOnMobile`: giấu popup, `showAfterSuccessBanner`, rồi `My()` gọi `triggerMobileRestore`.
- `onUserStartMobileRestore`: banner trước, rồi `My()` nếu còn session.

`My` nuốt mọi lỗi của `633`, chỉ log. `signalRestoreOnMobile` chỉ gửi `reqId`. `data.err` phải là 0 thì method coi là thành công. Lỗi đó không được phép kéo banner thành công xuống.

`armBackupDoneMonitor` khớp push `backup_done` theo `bkid` để ghi QoS. Hết 180 giây thì log `monitor_timeout` và hủy pending. Không dùng timeout này để quyết định hiện banner. `armBackupDoneWait` giữ tiến độ 99 và có nhánh hiện success khi timeout, nhưng không có caller trong bundle này. Đừng gắn success vào hàm đó.

## Tự chạy lại

`getResumeStepFor`: có `filePath` hoặc `bkid` thì bước `"upload"`, không thì `"engine"`. Biểu thức `checkpointOffset` bị bỏ qua bởi dấu phẩy, không đổi bước.

Interrupt state được ghi khi phiên đang chạy. `onServicesReady` khi cờ bật: nếu còn interrupt thì hiện banner lỗi `InterruptByDevice` và xóa bản ghi. Cờ tắt thì xóa bản ghi và giấu UI.

Auto-resume dùng `auto_resume_max_attempts`, cửa sổ, và backoff ở bảng trên. Đây không phải file `backup-session.json` của Rust cũ.

## Thứ tự nên viết, khi được yêu cầu

1. Cờ `enabled` và guard mở thẻ. Test bằng fixture, không gửi socket.
2. `634` rồi `632`, map đúng NONE / EXISTING / TTL local / popup. Vẫn chưa tạo phiên.
3. Spam và cooldown đúng chỗ ghi `recordRequest`.
4. `631` và muối local. Assert thiếu nonce thì ném, không có đường raw khi mã hóa bật.
5. Đọc handler `GET_BACKUP_NOISED_IDS` và `getSync2BackupFilePath` / `Xn.a.write`. Viết test fixture cho scope và cho một block protobuf, chưa upload.
6. Metadata và `630`, gồm cửa sổ `1740000` ms và mã `212`.
7. Upload một file fixture nhỏ, retry đúng ba lớp lỗi, checksum. Chưa bấm trên tài khoản sống.
8. Banner thành công trước `633`. `633` chỉ từ hai nút, lỗi bị nuốt.
9. Monitor `backup_done` chỉ cho QoS.
10. Phần trăm theo scope và weight. Không có số phần trăm giả.

Wasm backup chỉ được nạp từ file `assets/wasm/libzproto_backup_wasm_bg.042497d03f0f504b6b1444e25a7db0d9.wasm`. Không dùng wasm Sync 2 cho sao lưu. Không khôi phục crate `zalo-sync2` nguyên trạng. Crate đó đã trộn protobuf tự viết với host wasm và đã bị xóa cùng máy cũ.
