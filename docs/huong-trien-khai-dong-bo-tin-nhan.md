# Hướng triển khai lại đồng bộ tin nhắn (điện thoại → PC)

Ngày đọc nguồn: 2026-11-21. Đây là đặc tả để viết lại từ đầu. Chưa có mã Rust cho phần này. Không sửa máy cũ: máy cũ đã gỡ, xem `docs/bao-cao-dong-bo-tin-nhan-sai.md`.

Không đụng Sao lưu tin nhắn (`630`–`634`) và không đụng Xuất / Nhập zip.

## Nguồn phải bám

Đọc các file này trong `giao-dien/`. Số dòng dưới là offset trong bản pretty-print đang có trên máy, dùng để tìm lại, không phải API ổn định.

| Việc | File |
| --- | --- |
| Số lệnh socket | `02-js/compact-app/modules/compact2101.js` — `SYNC_MESSAGE` |
| Enum Sync 2 | `02-js/compact-app/modules/compact2186.js` — module `SCpg` |
| Gửi socket `590` / `591` / `592` | `02-js/compact-app/modules/compact3857.js` |
| Bọc retry | `02-js/lazy/main-startup.fe4e52efd649575b333b.js` — class `Kte`, hàm `Qte` |
| Máy trạng thái `sync-full` | `02-js/compact-app/modules/compact0909.js` — `function N`, class `M` |
| Ai gọi đồng bộ tay / tự động | cùng file `main-startup`, `manualSync` và `autoSyncWhenLogin` |
| Bộ lọc control | cùng file, `gne` … `One`, class xử lý control |
| Fallback số remote | cùng file, object `jae`, `Uae`, `zae`, namespace `sync_2_pc` |
| Trang danh sách hội thoại | `02-js/compact-app/modules/compact3613.js` — class macro, `for (; m && …)` |
| Tham số builder | `02-js/compact-app/modules/compact3553.js` |
| Pha tin còn lại, phần trăm | `02-js/compact-app/modules/compact3272.js` |
| Phiên, query, hasMore | `01-goc/pc-dist/shared-worker.d6af5465eb38c618bfe0.js` |
| Ưu tiên renderer | `compact4376.js` (`vuBx`) qua `compact1951.js` (`OzWg`) và `compact2997.js` (`czbv`) |
| HTTP `12888` | `compact3178.js` định nghĩa, `compact0598.js` chỉ là catalog. Không có caller. |
| Logger | `compact2331.js`. Không port thành giao thức. |

`compact-app-pc` và `shared-worker` là cùng một bản PC. `10-ban-day-du` là bản copy. Đối chiếu trên `02-js/compact-app/modules` và `01-goc/pc-dist/shared-worker`.

## Việc không được làm lại

- Không một vòng `drive()` vừa xin danh sách vừa kéo tin, đóng `591` rồi mới gửi phiên sau.
- Không nhét mọi hội thoại vào một khung. Lần đó server đóng WebSocket `1009`.
- Không gửi pha tin dạng 30 query `GetAll`, mỗi query limit `2147483647`.
- Không gắn cứng trang `100`, confirm `180` giây, batch-gap `120` giây, wake `20` giây. Những số đó là của worker đã xóa.
- Không hiện «Điện thoại đã đồng ý, đang chờ dữ liệu…» rồi chặn đến khi có lô. Câu đó không có trong Electron.
- Không bịa phần trăm khi chưa có khoảng tin `isCompleted`.
- Không gọi HTTP `request-sync`, `pull_mobile_msg`, `cancel_pull_mobile_msg` cho đường Sync 2.
- Không dùng wasm backup (`libzproto_backup_wasm_bg`) để sinh `ek` hay giải tin Sync 2.
- Không bịa Noise, Trust, `encryptParam`, `zpw_kt`, `GET_BACKUP_NOISED_IDS`.
- Không gửi lệnh `633` trong máy đồng bộ. `633` là nút khôi phục của sao lưu.

## Khung socket

Mọi lệnh backup đang đi qua `Frame` của `core/zalo-client/src/listener.rs`:

- byte 0: version `1`
- byte 1–2: `cmd` little-endian
- byte 3: `subCmd`
- phần còn lại: JSON UTF-8

Đồng bộ tin nhắn dùng lại đúng `Frame` đó, `subCmd = 0`. Lượt này không tìm thấy encoder lệnh riêng trong compact (các `writeUInt16LE` gặp được là header ZIP). Không viết encoder thứ hai.

## Ba lệnh

`compact2101.js`:

| Tên | Cmd | Hàm gửi | JSON `data` của `requestAsyncV2` | Timeout socket mặc định |
| --- | --- | --- | --- | --- |
| `REQUEST` | 590 | `requestSyncMessage(e)` | `{ data: e }` | `10000` ms |
| `ACK_DELETE_SYNC_SESSION` | 591 | `ackDeleteSyncSession(e)` | `{ data: e }` | `3000` ms |
| `REQUEST_MOBILE_WAKE_UP` | 592 | `requestMobileWakeUp(e)` | `e` (không bọc thêm `data`) | `3000` ms |

`e` của 590 là object phiên ở mục sau. `e` của 591 và 592 là `{ syncId, toDevice, reason }` và `{ syncId, toDevice }`. `toDevice` mobile = `0`.

Retry bọc ngoài (`Wte`): tối đa 3 lần, delay `min(1000 * 2^(lần-1), 3000)` ms.

- 590 bỏ qua retry khi lỗi mạng `NO_NETWORK`, `TIMEOUT`, và mã `211`. Call site không đặt tên cho `211`.
- 591 bỏ qua retry khi `NO_NETWORK` và `TIMEOUT`.
- 592 dùng whitelist rỗng, nên retry mọi lỗi trong hạn 3 lần.

Trước khi gửi 590, Electron kiểm tra mạng và socket đang `OPEN`. Sai thì lỗi `NO_NETWORK`, không gửi.

## Enum `SCpg` (`compact2186.js`)

`syncType` trên dây của máy full là `SyncFull = 0`. `SyncOnDemand = 1` là đường khác, không trộn vào một request (worker từ chối nhiều `type` hoặc nhiều `priority` trong một request).

`toDevice`: Mobile `0`, Web `1`, PC `2`. Request full đặt Mobile.

Trạng thái điện thoại (`transfer_status.data.status`, `fromDevice` phải là Mobile):

| Tên | Số |
| --- | --- |
| Active | 1 |
| Idle | 2 |
| WaitingConfirm | 3 |
| Confirmed | 4 |
| BypassConfirmed | 5 |
| UserReject | 6 |
| UserCancel | 7 |
| MasterDeviceBusy | 8 |
| MasterDeviceLowStorage | 9 |
| MasterDeviceLogout | 10 |

Lý do đóng phiên (`591` field `reason`):

| Tên | Số |
| --- | --- |
| UserCancel | 0 |
| SyncSuccess | 1 |
| Error | 2 |
| OutDated | 3 |

`UserCancel` của status là 7. `UserCancel` của reason là 0. Không gửi 7 trong lệnh 591.

## Một phiên (`class Uo` trong shared-worker)

Mỗi phiên, trước khi gửi:

- `syncId = generateRandomString(32)` mới.
- `ek` mới từ `genSyncKeyPair()` của wasm Sync 2. Hàm wasm là `la()` trong `libzproto_wasm_bg.ecc0555ba9d0f8867a4699ce943ec745.wasm`. `ik` là identity key đã nạp, không tạo mới mỗi phiên.
- Dây gửi `ek` và `ik` là base64 của **public** key (`toBase64(ek.publicKey)`, `toBase64(ik.publicKey)`). Layout byte của public key do wasm đó xuất. Lượt này không dump wasm. Không chép layout từ crate key đã xóa, và không dùng xor key của wasm backup.
- `ussidx = nextIndex(userSyncSessionId)`: lấy chỉ số hiện tại (chưa có thì 0) rồi tăng. Không có field `userSyncSessionId` trên dây.
- `deviceName` từ helper thiết bị.
- `ver: 1`.
- `syncType: 0`.

Object gửi trong `data.data` của lệnh 590:

```text
{
  syncId, syncType: 0,
  ek, ik,
  toDevice: 0,
  tempKey,
  deviceName,
  req: {
    type,          // "GetAll" hoặc "PrepareAll"
    priority,      // số nguyên, xem mục ưu tiên
    queries: [ { partition, from, to, limit } ],
    batchSize      // transferBatchSize, số dòng mỗi lô upload
  },
  ver: 1,
  ussidx
}
```

Query trên dây chỉ có `partition`, `from`, `to`, `limit`. `database` và `order: "DESC"` là trường nội bộ của GetAll, không có trong object query socket.

`start()` đăng listener lô tin, lô meta, trạng thái điện thoại, rồi `requestSync()`. Promise trả về khi ACK của 590 đặt bước `Requested` (giá trị 1). Nó không đợi hết lô. Phiên sau được tạo khi phiên trước đã ACK, trong lúc phiên trước vẫn nhận lô. Không `release()` giữa các nhóm.

`tempKey` của manager: phiên đầu dùng `oneTimeKey` của lần mở database; mọi phiên sau trong cùng manager gửi `""` vì lúc `create` đọc `syncSessions.length ? "" : oneTimeKey` **trước** khi push.

`release(reason)` chỉ gửi 591 khi phiên đã `alreadyStart()`, body `{ syncId, toDevice, reason }`.

### Khi nào tái sử dụng một phiên

`canReuseThisSession`: partition trùng, limit mới ≤ limit cũ, khoảng mới nằm trong khoảng cũ (`from/to`, thiếu thì 0 … `Number.MAX_SAFE_INTEGER`), và (`limit cũ == 2147483647` hoặc `to` mới bằng `to` cũ), cộng quy tắc priority và scope lỗi trong hàm đó. Không mở 590 mới nếu phiên còn dùng được.

### Tiếp tục bên trong một lần GetAll

`executeGetAllQuery` lặp trong **cùng** query / cùng phiên:

- lần sau: `range.to = Number(lastRow[index]) - 1`
- `limit = limitGốc - số dòng đã có`
- dừng khi `!hasMore` hoặc đủ limit

Đây không phải lệnh 590 mới, và không phải cách renderer lật trang hội thoại.

### Nghe trạng thái trong phiên

Cùng `syncId`, trước khi fulfilled:

| Status | Việc |
| --- | --- |
| Active (1) | hẹn giờ batch |
| Confirmed (4) hoặc BypassConfirmed (5) | bước `Preparing`, hẹn giờ batch |
| WaitingConfirm (3) | hẹn giờ connect |
| Idle (2) | xóa hẹn giờ |

`alreadyConfirmed` khi bước hiện tại ≥ `Preparing`. Khi bất kỳ phiên nào đã confirmed, `connectTimeout` đưa xuống 0. Không thì `max(0, connectTimeout cấu hình − thời gian từ request đầu)`.

Default gắn trên session manager, rồi bị options của lần mở database ghi đè:

| Khóa | Default session manager |
| --- | --- |
| connectTimeout | 10000 ms |
| batchTimeout | 10000 ms |
| wakeupTimeout | 180000 ms |
| transferBatchSize | 2000 |

Wake (`592`) khi gọi `wakeup()`: gửi `{ syncId, toDevice }`. Mã trả về 0 thì hẹn `wakeupTimeout`.

## Ưu tiên trên dây

Enum renderer `vuBx` (`compact4376.js`): `HIGH = 0`, `NORMAL = 1`, `LOW = 2`.

Shared-worker có một enum TaskPriority khác (`HIGH = 1`, `NORMAL = 2`, `LOW = 3`). Số trên socket là số renderer copy vào query, không phải enum nội bộ đó.

| Pha | Nguồn | Số |
| --- | --- | --- |
| Danh sách hội thoại | `czbv.e` = `OzWg.c` = `TaskPriority.HIGH` | 0 |
| Preview | `czbv.j` = `OzWg.g` = HIGH | 0 |
| Prepare tin còn lại và pha other-messages | `czbv.f` = `OzWg.d` = LOW | 2 |

Builder một hội thoại trong `compact3553.js` class `l` ghi sẵn `priority: 1`. Đường sync-full other-messages không dùng số đó; nó truyền `priority: Lse.f` tức 2 vào task, task truyền tiếp vào query.

`req.batchSize` là `transferBatchSize` (fallback `2000`). Đó là số dòng mỗi lô upload, không phải số query, không phải cỡ trang hội thoại.

## Remote config `sync_2_pc`

Getter trả `remoteConfig.key(tên).value` hoặc `nestedKey`. Không có số trong thân getter. Số dưới là default đóng gói khi server không gửi khóa, object `jae` / `Uae` (manual) / `zae` (auto). Giá trị đang chạy trên tài khoản là giá trị remote lúc đó. Không gắn cứng fallback nếu remote đã có số.

`jae`:

| Khóa | Fallback |
| --- | --- |
| `batch_size` | 2000 |
| `conversations_batch_size` | 1000 |
| `transfer_batch_size` | 2000 |
| `max_number_of_concurrent_sync_items` | 4 |
| `max_percent_batch_fail` | 0.5 |
| `max_batch_fail` | 10 |
| `sync_conv_list_batch_max_attempts` | 3 |
| `sync_paused_timeout_ms` | 600000 |
| `sync_conv_list_update_ui_batch_size` | 1 |

`Uae` là `manual_sync`, `zae` là `auto_sync`. Phần timeout và trần của hai nhánh giống nhau trong bản này:

| Khóa | Fallback |
| --- | --- |
| `awaiting_mobile_timeout_ms` | 600000 |
| `wakeup_timeout` | 180000 |
| `batch_timeout` | 10000 |
| `sync_conv_list.max_time_range_d` | -1 |
| `sync_conv_list.max_number_of_conv` | -1 |
| `sync_last_chat.max_number_of_conv` | 10 |
| `sync_last_chat.max_number_of_msg_per_conv` | 1 |
| `sync_recent_chat.max_number_of_conv` | 10 |
| `sync_recent_chat.max_number_of_msg_per_conv` | 100 |
| `sync_all_remaining_msg.max_number_of_conv` | -1 |
| `sync_all_remaining_msg.max_number_of_msg_per_conv` | -1 |
| `sync_all_remaining_msg.max_time_range_d` | -1 |

`getTimeoutConfigs(MANUAL)` đọc nhánh `manual_sync`:

- `confirmTimeout` = `awaiting_mobile_timeout_ms` (fallback 600000)
- `wakeupTimeout` = `wakeup_timeout` (180000)
- `batchTimeout` = `batch_timeout` (10000)

Auto đọc nhánh `auto_sync`, cùng các số fallback. `cross_setting.cfTimeout = 180` trong `compact1847.js` là đường cũ, đơn vị giây. Không dùng 180 giây làm confirm của Sync 2.

Constructor `compact3553.js` còn `query.batchSize: 100` và `limit… = -1`. Đó là giá trị builder khi chưa bị remote ghi đè. Đường sync-full không gửi 100: nó ghi `clientBatchSize` từ `conversations_batch_size`.

## Máy trạng thái

Class `M` (`compact0909.js`):

- `tempKey` khởi tạo `""`.
- `startManualSync` không gán tempKey. `initNewMachineInfo` đặt `tempKey = e.tempKey ?? ""`. `manualSync` không truyền `tempKey`, nên lần bấm tay gửi tempKey rỗng. Điện thoại hiện xác nhận «Đồng bộ ngay».
- `startAutoSyncFull` gán `this.tempKey = e.tempKey` rồi gửi `START_AUTO_SYNC`.
- Cả `START_MANUAL_SYNC` và `START_AUTO_SYNC` đều vào trạng thái tên `manual_sync` của máy `init`.
- `resendRequestSync` tạo `userSyncSessionId` mới, source = `RETRY_ON_WAITING_FOR_CONFIRMATION_POPUP` (10), gửi `RESEND_REQUEST_SYNC`.

`userSyncSessionId` do `qiiv` sinh khi bắt đầu một lần sync. Nó không lên dây; chỉ để tính `ussidx`.

### Cửa tempKey sau đăng nhập

Không lấy tempKey từ `transfer_sync2`.

Predicate `gne`: `act === "transfer_after_login"` và `act_type === "transfer_msg"` và `temp_key` khác rỗng và `data.imei === getZaloClientID()`. Rồi `getLatestValidTransferAfterLoginEvent`: lấy sự kiện mới nhất, chỉ hợp lệ khi `now - ts <= 600000`.

Chuỗi sau đó: lưu tempKey, `checkAndTriggerAutoSync`. Nếu tin đã sẵn sàng thì không sync. Nếu worker sẵn và tin chưa sẵn thì `sync(AUTO_SYNC, tempKey)` → `startAutoSyncFull`. Có thể hoãn nếu migration DB còn chạy.

`RECEIVE_TEMP_KEY` có trong enum sự kiện của `compact0909.js` và không có transition dùng nó trong máy đã đọc.

### Đồ thị `sync-full`

`initial: determining_initial_sync_state`. `always`:

1. `isMessageReady` → `success`
2. `isMissingConversations` → `sync_conversations`
3. `isMissingRecentAndPreviewMessages` → `prepare_sync_preview_and_recent_messages`
4. còn lại → `sync_other_messages`

Tiếp theo:

- `sync_conversations` xong: còn thiếu tin khác thì `sync_other_messages`, không thì `sync_preview_messages`. Lỗi → `failure`.
- prepare preview/recent: cả xong lẫn lỗi đều sang `sync_preview_messages`.
- preview xong → `sync_other_messages`. Lỗi → `failure`.
- other xong → `success`. Lỗi → `failure`.
- `RESEND_REQUEST_SYNC` (global) → `resend_request_sync` → `disposeSync` với reason `UserCancel` (0) → trở lại `determining_initial_sync_state`.

`USER_CONFIRM`, `MOBILE_BYPASS_CONFIRM`, `MOBILE_IDLE`, `MOBILE_ACTIVE` trong các trạng thái trên chỉ đổi UI. Chúng không gửi thêm 590.

`handleMobileStatus` map enum sang sự kiện máy. `UserCancel` (status 7) đặt `cancelReason = MOBILE_USER_CANCEL` và `CANCEL_SYNC`. Handler cancel gọi `cancelSync(DisposeReason.UserCancel)` tức reason **0**.

`handleFailure` gọi `cancelSync(Error)` tức reason **2**. `handleSuccess` của máy ngoài gọi `disposeSync({ reason: SyncSuccess })` tức reason **1**, sau khi pha hội thoại đã đóng database hội thoại một lần (cũng reason 1). Hai lần reason 1 này khác nhau: lần đầu đóng database hội thoại, lần cuối đóng phần còn lại của cả lần sync.

`MOBILE_BUSY`, `MOBILE_LOW_STORAGE`, `MOBILE_LOGOUT` đưa máy ra `failure` (rồi reason 2).

### Control trên socket 601 / 603

`act_type`:

- `transfer_sync2` → xử lý Sync 2.
- `transfer_msg` → Sync 2 nếu feature bật, không thì đường Sync 1 cũ.

`act`:

| act | Điều kiện thêm | Việc |
| --- | --- | --- |
| `transfer_after_login` | predicate `gne` (act_type `transfer_msg`, temp_key, imei, 10 phút) | auto sync |
| `transfer_status` | `fromDevice === 0` và status 1–10 như bảng | `handleMobileStatus` |
| `upload_batch` | `data.err === 0` | lô backup info của sync |
| `upload_batch` với `err !== 0`, hoặc `transfer_error` | | lỗi sync |

Một control không được làm worker trả Ok hay gửi 591 chỉ vì «có tín hiệu». Trạng thái 3 là đang chờ bấm trên điện thoại. Trạng thái 4 và 5 mới là đã xác nhận và chuyển phiên sang Preparing.

## Danh sách hội thoại

Macro `compact3613.js`, sau khi prepare conversation (nếu có) và sau khi ghi khoảng thiếu lần đầu nếu chưa từng sync thành công:

Vòng `for (; hasMore && (unlimited || fetched < cap);)`:

- `cap` = `limitConversationsToSync`. Âm nghĩa là không trần (`manual` fallback `-1`). `cap === 0` ở tầng worker thì trả rỗng, `hasMore` false, không query.
- Cỡ trang `n` bắt đầu bằng `query.clientBatchSize` = remote `conversations_batch_size` (fallback 1000).
- Trang đầu, nếu `limitConversationsToSyncPreview > 0`, thay `n` bằng số đó. Fallback preview là `sync_last_chat.max_number_of_conv = 10`.
- Nếu có trần, `n = min(n, cap - fetched)`.
- Gọi lấy một trang với `thresholdRange = { fromTs, toTs }` và `limitConversationsToSync = n`.
- Worker: limit trên dây = limit đó (dương). Query `GetAll`, **partition rỗng**, `from` / `to` là mốc thời gian, priority 0. `hasMore` khi `rows.length === limit` (bằng, không phải `>=`).
- Options mở database: `connectTimeout = confirmTimeout` remote, cùng wakeup và batch remote, `transferBatchSize` remote.
- Trang có dòng: `toTs` vòng sau = `lastTs` của **hội thoại cuối**, không trừ 1. `fromTs` giữ `max(lastSyncConvSuccessTs, range.from)`.
- Trang 0 dòng mà `hasMore`: dừng và tắt `hasMore` (log «breaking pagination»).
- Mỗi trang còn validate, import, cập nhật preview. Prepare preview/recent chạy một lần trong trang nếu config bật. Đó là việc thêm, không phải một 590 danh sách mới cho mỗi trang.
- Trang lỗi: thử lại tối đa `sync_conv_list_batch_max_attempts` (fallback 3), delay `min(1000 * 2^(lần-1), 3000)` ms, chỉ với mã mà helper `Y` coi là retry được. Lượt này không liệt whitelist mã đó.

Hết vòng, **chỉ khi success**:

1. `deleteMissingMessageRanges` của các khoảng hội thoại đã xin
2. `setLastSyncConvSuccessTs(now)`
3. `reportBackupStatus` (logger `compact2331.js`)
4. `closeConversationRemoteDatabase({ reason: SyncSuccess })` một lần

Không gửi 591 giữa các trang. Database hội thoại mở bằng `ensureConversationDatabaseOpen` một lần và giữ đến bước 4.

`limit` 0 trên query GetAll bị đổi thành `2147483647` (`mo`) trong `createConversationQuery` chỉ khi số đưa vào không dương. Trang sync-full đưa số dương, nên limit trên dây là cỡ trang, không phải `mo`.

## Pha tin

Other-messages (`compact3272.js` `execute`): **prepare trước**, rồi mới `syncAllMessages`.

### Prepare

`prepareSyncOtherMessages` trong shared-worker:

- mọi khoảng thiếu trong `thresholdRange` bị gắn `limit: 2147483647`
- bỏ khoảng không có noised id (`void 0` thì `continue`)
- `executePrepareMessageQueries`
- `buildPrepareQuery` nhóm theo priority, nhét query vào nhóm đang có nếu `getQueryCount() < 30`, không thì nhóm mới
- mỗi nhóm `prepareSession` → một phiên, `type` là `PrepareAll`
- `await` ACK của nhóm rồi mới nhóm sau; phiên trước không bị `release`

Partition một hội thoại: `` `${isGroup ? "group" : "oneone"}/${globalNoisedId}` ``. Nhóm khi id PC bắt đầu bằng `g`.

### Đọc tin từng hội thoại

`syncAllMessages` đếm `totalCount = countAllMissingRanges` rồi chạy task.

`fillProcessingSlots` lấy `max_number_of_concurrent_sync_items` (fallback 4) và đổ từng đó task. Task xong thì lấp tiếp. `onNeedMoreRanges` tải `getMaxFailedBatches() + 1` khoảng (fallback 10 + 1 = 11). `hasMore` của bước nạp là `số khoảng >= số đó`.

Mỗi task một hội thoại, một `executeMessageQuery`:

- limit = `config.batchSize` = remote `batch_size` (fallback 2000). Class `l` còn hạ `batchSize` nếu nó lớn hơn limit dương của chính query.
- priority 2 trên đường sync-full
- một partition
- `range` from/to của khoảng thiếu
- `limit === 0` thì worker trả success, `hasMore` false, không query
- không có noised id thì lỗi `PC_GET_NOISED_ID_FAILED`
- `hasMore = rows.length >= limit`
- 0 dòng: success, `hasMore` false, không viết lại `toTs`
- có dòng: `toTs = timestamp` của dòng cuối (không trừ 1). `isCompleted` false nếu `hasMore` và còn `lastTs`, để cùng hội thoại bị hỏi tiếp với trần mới

`checkBatchFailPercentage`: nếu `failed / (failed + successful) > max_percent_batch_fail` (fallback 0.5) thì ném `EXCEEDED_MAX_PERCENT_BATCH_FAIL`. Mẫu số 0 thì bỏ qua.

### Phần trăm

`handleTaskCompleted`: nếu `!e.isCompleted` thì return, không tăng. Khi một khoảng thực sự xong: `completedCount++`, `floor(completedCount / totalCount * 100)`. Chỉ lúc đó UI mới có số %. Trước đó là trạng thái thật (đang chờ xác nhận, đang nhận), không phải đồng hồ giả.

## Wasm và giải mã lô

File Sync 2: `assets/wasm/libzproto_wasm_bg.ecc0555ba9d0f8867a4699ce943ec745.wasm` (đúng tên trong shared-worker).

File backup: `libzproto_backup_wasm_bg.042497d03f0f504b6b1444e25a7db0d9.wasm`. File này ở trên đĩa. Máy sao lưu đã bị gỡ, không file `.rs` nào nạp nó. Đồng bộ không được gọi wasm backup.

`decryptChunk`:

- meta: `decryptMetaData(cipher, ek.privateKey)`, rồi giữ `sessionRecord`
- tin: đợi session record, `decryptMessage(cipher, sessionRecord)`

Chưa port host wasm Sync 2 sang Rust. Lần viết lại phải gọi đúng export của file sync, sau khi đọc chữ ký trong wasm. API `BackupCrypto` của máy sao lưu cũ đã bị xóa. Không chép API đó sang đồng bộ.

## Thứ tự nên viết

1. Parser control đúng bảng `act` / `act_type` / `fromDevice`. Chưa gửi 590.
2. Một phiên: sinh `syncId`, `ek`, `ussidx`, đóng gói object 590, `start()` trả về ở ACK. Test bằng fixture JSON, không gửi lên tài khoản sống.
3. Database hội thoại: nhiều trang, một lần `close` reason 1 ở cuối, không 591 giữa trang. Assert biên `toTs = lastTs`.
4. Prepare `< 30` kiểu `PrepareAll`, limit `mo`, phiên chồng sau ACK.
5. GetAll một hội thoại, limit = `batch_size`, `hasMore` bằng `>=`, trần sau = `lastTs`.
6. Phần trăm chỉ khi `isCompleted`.
7. UI và chuỗi catalog sau cùng, khi đã có lô thật. Không thêm thẻ Cài đặt trước bước 3.

Chưa chạy các bước đó trong lượt viết tài liệu này.
