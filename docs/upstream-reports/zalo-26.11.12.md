# Upstream Zalo Desktop Analysis: `26.11.12`

- **Pinned in `protocol.json`**: `26.10.20`
- **Windows latest**: `26.11.12` (https://res-download-pc.zadn.vn/win/ZaloSetup-26.11.12.exe)
- **macOS latest**: `26.11.10` (https://res-download-pc.zadn.vn/mac/ZaloSetup-universal-26.11.10.dmg)
- **Analyzed platform**: `windows` (`https://res-download-pc.zadn.vn/win/ZaloSetup-26.11.12.exe`)

## Summary

- **Classification**: `feature-update`
- **`client_version` (`zpw_ver`)**: pinned `694` → detected `697` (changed: `True`)
- **`ParamCipher` key unchanged**: `True`
- **WASM crypto modules changed**: `True`
- **New `/api/...` endpoints not in `core/zalo-client`**: `284`
- **New `STR_*` UI keys not in `core/zalo-design`**: `66`
- **Bundles scanned**: `1` ASAR archive(s), `118` JS file(s)

## WASM Crypto Modules

- `libzproto_backup_wasm_bg.042497d03f0f504b6b1444e25a7db0d9.wasm` (`sha256: 3a980db6271ff758...`) — **MATCH**
- `libzproto_wasm_bg.5ea0fb2c9af4a45bc7882b71307b6f83.wasm` (`sha256: 398a2016ede72d1e...`) — **NEW/CHANGED**

## New API Endpoints (sample)

- `/api/3/minidump/`
- `/api/action/query`
- `/api/aifeatures/translate/v1/auto`
- `/api/aifeatures/translate/v1/manual`
- `/api/aifeatures/translate/v1/setting`
- `/api/aifeatures/translate/v1/suggest`
- `/api/aifeatures/voice2text`
- `/api/alias/list/v2`
- `/api/banner/v1/info`
- `/api/board/listReminder`
- `/api/board/multi_unpin`
- `/api/board/oneone/update`
- `/api/board/personal/delete`
- `/api/board/personal/get`
- `/api/board/personal/list`
- `/api/board/personal/todo/create`
- `/api/board/personal/todo/ding`
- `/api/board/personal/todo/dingack`
- `/api/board/personal/todo/status`
- `/api/board/personal/todo/update`
- `/api/board/personal/todo/verify`
- `/api/board/pin`
- `/api/board/pin/list`
- `/api/board/pin/update`
- `/api/board/topic/create`
- `/api/board/topic/update`
- `/api/board/unpin`
- `/api/chk`
- `/api/cm/getpath`
- `/api/cm/mget`
- `/api/cm/pc/dl`
- `/api/cm/rgetv2`
- `/api/conv/blocksharing/getConvers`
- `/api/conv/blocksharing/updateConvers`
- `/api/conv/cm/grOfl`
- `/api/conv/getUnreadMark`
- `/api/convlabel/manage/add`
- `/api/convlabel/manage/delete`
- `/api/convlabel/manage/reorder`
- `/api/convlabel/manage/update`

## New UI String Keys `STR_*` (sample — indicates new features)

- `STR_AI_TRANS_CONSENT_AGREE: Đồng ý`
- `STR_AI_TRANS_CONSENT_CANCEL: Hủy`
- `STR_AI_TRANS_CONSENT_DESC: Để tạo bản dịch, bạn cần cho phép Zalo AI xử lý tin nhắn trong trò chuyện này.`
- `STR_AI_TRANS_CONSENT_LEARN_MORE: Tìm hiểu thêm`
- `STR_AI_TRANS_CONSENT_TITLE: Cho phép Zalo AI xử lý tin nhắn?`
- `STR_AI_TRANS_COPY_ORIGINAL_MESSAGE: Copy tin nhắn gốc`
- `STR_AI_TRANS_COPY_TRANSLATION: Copy bản dịch`
- `STR_AI_TRANS_FEATURE_UNAVAILABLE: Tính năng hiện không khả dụng. Vui lòng thử lại sau.`
- `STR_AI_TRANS_HIDE_TRANSLATION: Ẩn bản dịch`
- `STR_AI_TRANS_LANG_EN: Anh`
- `STR_AI_TRANS_LANG_FULL_EN: tiếng Anh`
- `STR_AI_TRANS_LANG_FULL_KO: tiếng Hàn`
- `STR_AI_TRANS_LANG_FULL_VI: tiếng Việt`
- `STR_AI_TRANS_LANG_FULL_ZH_HANS: tiếng Trung`
- `STR_AI_TRANS_LANG_KO: Hàn`
- `STR_AI_TRANS_LANG_VI: Việt`
- `STR_AI_TRANS_LANG_ZH_HANS: Trung`
- `STR_AI_TRANS_MSGINFO_AUTO_OFF: Đã tắt dịch tin nhắn nhận.`
- `STR_AI_TRANS_MSGINFO_AUTO_ON: Đã bật dịch tin nhắn nhận.`
- `STR_AI_TRANS_MSGINFO_GRACE_PERIOD: Gói zCloud của bạn đang tạm hết hạn. Dịch tự động đã tắt cho đến khi gia hạn.`
- `STR_AI_TRANS_MSGINFO_SETTINGS: Cài đặt`
- `STR_AI_TRANS_MSGINFO_SUGGEST_OFF: Đã tắt gợi ý bản dịch.`
- `STR_AI_TRANS_MSGINFO_SUGGEST_ON: Đã bật gợi ý bản dịch.`
- `STR_AI_TRANS_PAIR_LABEL: $0$ - $1$`
- `STR_AI_TRANS_PROMOTE_AUTO_CTA: Dịch trò chuyện`
- `STR_AI_TRANS_PROMOTE_AUTO_DESC: Chỉ với một lần thiết lập, trò chuyện dễ dàng hơn`
- `STR_AI_TRANS_PROMOTE_AUTO_TITLE: Tự động dịch trò chuyện này với Zalo AI?`
- `STR_AI_TRANS_RETRY_TRANSLATION: Thử dịch lại`
- `STR_AI_TRANS_SEND_APPLY: Nhấn để dùng bản dịch này`
- `STR_AI_TRANS_SEND_CLOSE: Đóng gợi ý bản dịch`
- `STR_AI_TRANS_SEND_COPY_SELECTED_TRANSLATION: Copy nội dung dịch đã chọn`
- `STR_AI_TRANS_SEND_ERR_GENERIC: Đã xảy ra lỗi.`
- `STR_AI_TRANS_SEND_ERR_OFFLINE: Không có kết nối mạng`
- `STR_AI_TRANS_SEND_USE: Dùng`
- `STR_AI_TRANS_SETTINGS_AUTO: Dịch tin nhắn nhận`
- `STR_AI_TRANS_SETTINGS_AUTO_TIP: Tự động dịch tin nhắn mới của trò chuyện này`
- `STR_AI_TRANS_SETTINGS_DIRECTION: Dịch từ $0$ sang $1$`
- `STR_AI_TRANS_SETTINGS_LANG_PAIR: Cặp ngôn ngữ`
- `STR_AI_TRANS_SETTINGS_MODAL_DESC: Thiết lập tính năng dịch cho trò chuyện $0$:`
- `STR_AI_TRANS_SETTINGS_MODAL_DESC_NO_NAME: Thiết lập tính năng dịch cho trò chuyện này:`
- `STR_AI_TRANS_SETTINGS_MODAL_TITLE: Cài đặt dịch trò chuyện`
- `STR_AI_TRANS_SETTINGS_SUGGEST: Gợi ý bản dịch khi soạn tin nhắn`
- `STR_AI_TRANS_SETTINGS_SUGGEST_TIP: Soạn tin nhắn theo ngôn ngữ của trò chuyện này`
- `STR_AI_TRANS_SETTINGS_TITLE: Dịch trò chuyện`
- `STR_AI_TRANS_SETTINGS_UPDATE_ERROR: Đã xảy ra lỗi. Vui lòng thử lại.`
- `STR_AI_TRANS_TARGET_LANGUAGE: Chọn ngôn ngữ dịch`
- `STR_AI_TRANS_TARGET_LANG_EN: Tiếng Anh`
- `STR_AI_TRANS_TARGET_LANG_KO: Tiếng Hàn`
- `STR_AI_TRANS_TARGET_LANG_ZH_HANS: Tiếng Trung`
- `STR_AI_TRANS_TRANSLATED_BY_ZALO_AI: Dịch bởi Zalo AI`
- `STR_AI_TRANS_TRANSLATE_MESSAGE: Dịch `
- `STR_AI_TRANS_TRANSLATING: Đang dịch`
- `STR_AI_TRANS_TRANSLATION_FAILED: Có lỗi xảy ra.`
- `STR_AI_TRANS_URL_UNAVAILABLE: Url không khả dụng`
- `STR_AI_TRANS_ZCLOUD_UPSELL_AUTO_DESC: Một lần thiết lập, dịch mọi tin nhắn mới`
- `STR_AI_TRANS_ZCLOUD_UPSELL_AUTO_TITLE: Tự động dịch toàn bộ trò chuyện`
- `STR_AI_TRANS_ZCLOUD_UPSELL_SUGGEST_DESC: Nói cùng ngôn ngữ với đối phương, giữ nhịp trò chuyện luôn liền mạch`
- `STR_AI_TRANS_ZCLOUD_UPSELL_SUGGEST_TITLE: Gợi ý bản dịch khi soạn tin`
- `STR_AI_TRANS_ZCLOUD_UPSELL_TITLE: Trợ lý dịch Zalo AI, đặc quyền gói zCloud+`
- `STR_AI_TRANS_ZCLOUD_UPSELL_UPGRADE_BTN: Dịch trò chuyện với zCloud+`
- `STR_OCR_COPY_SELECTED_CONTENT: Copy nội dung đã chọn`
- `STR_RTF_TOOLTIP_WITH_HOTKEY: $0$ ($1$)`
- `STR_TOAST_RECEIVER_BLOCKS_STRANGERS: Yêu cầu thực hiện không thành công vì $0$ không nhận tin nhắn từ người lạ`
- `STR_TURN_OFF_2FA_GUIDE_ON_MOBILE: Để tắt, hãy mở Zalo trên điện thoại, vào $0$`
- `STR_TURN_OFF_2FA_GUIDE_PATH: Cài đặt → Tài khoản và bảo mật`
- `STR_TURN_OFF_2FA_GUIDE_TITLE: Zalo PC chưa hỗ trợ tắt bảo mật 2 lớp`

## Verification Checklist

- [ ] Verify `protocol.json` (`zalo_desktop_version`, `client_version`, `user_agent`) and `core/zalo-protocol/src/param_cipher.rs`.
- [ ] Verify `assets/wasm/libzproto_*.wasm` hashes & exports in `core/zalo-sync` and `core/zalo-backup`.
- [ ] Review new API endpoints and `STR_*` keys to port any high-value features to `core/zalo-client` & `zalo-linux/crates/zalo-gpui`.
- [ ] Keep `README.md` synchronized with any new user-facing features or configuration changes.
- [ ] Pass `cargo test --workspace -- --test-threads=1` across Linux, macOS, and Windows CI.
