# Upstream Zalo Desktop Analysis: `26.11.20`

- **Pinned in `protocol.json`**: `26.11.12`
- **Windows latest**: `26.11.12` (https://res-download-pc.zadn.vn/win/ZaloSetup-26.11.12.exe)
- **macOS latest**: `26.11.20` (https://res-download-pc.zadn.vn/mac/ZaloSetup-universal-26.11.20.dmg)
- **Analyzed platform**: `macos` (`https://res-download-pc.zadn.vn/mac/ZaloSetup-universal-26.11.20.dmg`)

## Summary

- **Classification**: `feature-update`
- **`client_version` (`zpw_ver`)**: pinned `697` → detected `698` (changed: `True`)
- **`ParamCipher` key unchanged**: `True`
- **WASM crypto modules changed**: `True`
- **New `/api/...` endpoints not in `core/zalo-client`**: `222`
- **New `STR_*` UI keys not in `core/zalo-design`**: `66`
- **Bundles scanned**: `1` ASAR archive(s), `117` JS file(s)

## WASM Crypto Modules

- `libzproto_backup_wasm_bg.042497d03f0f504b6b1444e25a7db0d9.wasm` (`sha256: 3a980db6271ff758...`) — **MATCH**
- `libzproto_wasm_bg.5ea0fb2c9af4a45bc7882b71307b6f83.wasm` (`sha256: 398a2016ede72d1e...`) — **NEW/CHANGED**

## New API Endpoints (sample)

- `/api/3/minidump/`
- `/api/4509632503087104/envelope/`
- `/api/action/query`
- `/api/aifeatures/translate/v1/auto`
- `/api/aifeatures/translate/v1/manual`
- `/api/aifeatures/translate/v1/setting`
- `/api/aifeatures/translate/v1/suggest`
- `/api/aifeatures/voice2text`
- `/api/alias/list/v2`
- `/api/banner/v1/info`
- `/api/board/pin`
- `/api/board/topic/create`
- `/api/board/topic/update`
- `/api/board/unpin`
- `/api/chk`
- `/api/conv/cm/grOfl`
- `/api/devices/linked`
- `/api/e2ee/pc/group/`
- `/api/e2ee/pc/group/contact`
- `/api/e2ee/pc/group/deleteConv`
- `/api/e2ee/pc/group/deleteMsg`
- `/api/e2ee/pc/group/file`
- `/api/e2ee/pc/group/forward`
- `/api/e2ee/pc/group/gif`
- `/api/e2ee/pc/group/link`
- `/api/e2ee/pc/group/mention`
- `/api/e2ee/pc/group/multiUndoMsg`
- `/api/e2ee/pc/group/photo`
- `/api/e2ee/pc/group/quote`
- `/api/e2ee/pc/group/sms`
- `/api/e2ee/pc/group/sticker`
- `/api/e2ee/pc/group/undoMsg`
- `/api/e2ee/pc/message/`
- `/api/e2ee/pc/message/contact`
- `/api/e2ee/pc/message/delete`
- `/api/e2ee/pc/message/deleteconver`
- `/api/e2ee/pc/message/file`
- `/api/e2ee/pc/message/forward`
- `/api/e2ee/pc/message/gif`
- `/api/e2ee/pc/message/link`

## New UI String Keys `STR_*` (sample — indicates new features)

- `STR_AI_TRANS_CONSENT_AGREE: \u0110\u1ed3ng \xfd`
- `STR_AI_TRANS_CONSENT_CANCEL: H\u1ee7y`
- `STR_AI_TRANS_CONSENT_DESC`
- `STR_AI_TRANS_CONSENT_LEARN_MORE: T\xecm hi\u1ec3u th\xeam`
- `STR_AI_TRANS_CONSENT_TITLE: Cho ph\xe9p Zalo AI x\u1eed l\xfd tin nh\u1eafn?`
- `STR_AI_TRANS_COPY_ORIGINAL_MESSAGE: Copy tin nh\u1eafn g\u1ed1c`
- `STR_AI_TRANS_COPY_TRANSLATION: Copy b\u1ea3n d\u1ecbch`
- `STR_AI_TRANS_FEATURE_UNAVAILABLE: T\xednh n\u0103ng hi\u1ec7n kh\xf4ng kh\u1ea3 d\u1ee5ng. Vui l\xf2ng th\u1eed l\u1ea1i sau.`
- `STR_AI_TRANS_HIDE_TRANSLATION: \u1ea8n b\u1ea3n d\u1ecbch`
- `STR_AI_TRANS_LANG_EN: Anh`
- `STR_AI_TRANS_LANG_FULL_EN: ti\u1ebfng Anh`
- `STR_AI_TRANS_LANG_FULL_KO: ti\u1ebfng H\xe0n`
- `STR_AI_TRANS_LANG_FULL_VI: ti\u1ebfng Vi\u1ec7t`
- `STR_AI_TRANS_LANG_FULL_ZH_HANS: ti\u1ebfng Trung`
- `STR_AI_TRANS_LANG_KO: H\xe0n`
- `STR_AI_TRANS_LANG_VI: Vi\u1ec7t`
- `STR_AI_TRANS_LANG_ZH_HANS: Trung`
- `STR_AI_TRANS_MSGINFO_AUTO_OFF: \u0110\xe3 t\u1eaft d\u1ecbch tin nh\u1eafn nh\u1eadn.`
- `STR_AI_TRANS_MSGINFO_AUTO_ON: \u0110\xe3 b\u1eadt d\u1ecbch tin nh\u1eafn nh\u1eadn.`
- `STR_AI_TRANS_MSGINFO_GRACE_PERIOD`
- `STR_AI_TRANS_MSGINFO_SETTINGS: C\xe0i \u0111\u1eb7t`
- `STR_AI_TRANS_MSGINFO_SUGGEST_OFF: \u0110\xe3 t\u1eaft g\u1ee3i \xfd b\u1ea3n d\u1ecbch.`
- `STR_AI_TRANS_MSGINFO_SUGGEST_ON: \u0110\xe3 b\u1eadt g\u1ee3i \xfd b\u1ea3n d\u1ecbch.`
- `STR_AI_TRANS_PAIR_LABEL: $0$ - $1$`
- `STR_AI_TRANS_PROMOTE_AUTO_CTA: D\u1ecbch tr\xf2 chuy\u1ec7n`
- `STR_AI_TRANS_PROMOTE_AUTO_DESC: Ch\u1ec9 v\u1edbi m\u1ed9t l\u1ea7n thi\u1ebft l\u1eadp, tr\xf2 chuy\u1ec7n d\u1ec5 d\xe0ng h\u01a1n`
- `STR_AI_TRANS_PROMOTE_AUTO_TITLE: T\u1ef1 \u0111\u1ed9ng d\u1ecbch tr\xf2 chuy\u1ec7n n\xe0y v\u1edbi Zalo AI?`
- `STR_AI_TRANS_RETRY_TRANSLATION: Th\u1eed d\u1ecbch l\u1ea1i`
- `STR_AI_TRANS_SEND_APPLY: Nh\u1ea5n \u0111\u1ec3 d\xf9ng b\u1ea3n d\u1ecbch n\xe0y`
- `STR_AI_TRANS_SEND_CLOSE: \u0110\xf3ng g\u1ee3i \xfd b\u1ea3n d\u1ecbch`
- `STR_AI_TRANS_SEND_COPY_SELECTED_TRANSLATION: Copy n\u1ed9i dung d\u1ecbch \u0111\xe3 ch\u1ecdn`
- `STR_AI_TRANS_SEND_ERR_GENERIC: \u0110\xe3 x\u1ea3y ra l\u1ed7i.`
- `STR_AI_TRANS_SEND_ERR_OFFLINE: Kh\xf4ng c\xf3 k\u1ebft n\u1ed1i m\u1ea1ng`
- `STR_AI_TRANS_SEND_USE: D\xf9ng`
- `STR_AI_TRANS_SETTINGS_AUTO: D\u1ecbch tin nh\u1eafn nh\u1eadn`
- `STR_AI_TRANS_SETTINGS_AUTO_TIP: T\u1ef1 \u0111\u1ed9ng d\u1ecbch tin nh\u1eafn m\u1edbi c\u1ee7a tr\xf2 chuy\u1ec7n n\xe0y`
- `STR_AI_TRANS_SETTINGS_DIRECTION: D\u1ecbch t\u1eeb $0$ sang $1$`
- `STR_AI_TRANS_SETTINGS_LANG_PAIR: C\u1eb7p ng\xf4n ng\u1eef`
- `STR_AI_TRANS_SETTINGS_MODAL_DESC: Thi\u1ebft l\u1eadp t\xednh n\u0103ng d\u1ecbch cho tr\xf2 chuy\u1ec7n $0$:`
- `STR_AI_TRANS_SETTINGS_MODAL_DESC_NO_NAME: Thi\u1ebft l\u1eadp t\xednh n\u0103ng d\u1ecbch cho tr\xf2 chuy\u1ec7n n\xe0y:`
- `STR_AI_TRANS_SETTINGS_MODAL_TITLE: C\xe0i \u0111\u1eb7t d\u1ecbch tr\xf2 chuy\u1ec7n`
- `STR_AI_TRANS_SETTINGS_SUGGEST: G\u1ee3i \xfd b\u1ea3n d\u1ecbch khi so\u1ea1n tin nh\u1eafn`
- `STR_AI_TRANS_SETTINGS_SUGGEST_TIP: So\u1ea1n tin nh\u1eafn theo ng\xf4n ng\u1eef c\u1ee7a tr\xf2 chuy\u1ec7n n\xe0y`
- `STR_AI_TRANS_SETTINGS_TITLE: D\u1ecbch tr\xf2 chuy\u1ec7n`
- `STR_AI_TRANS_SETTINGS_UPDATE_ERROR: \u0110\xe3 x\u1ea3y ra l\u1ed7i. Vui l\xf2ng th\u1eed l\u1ea1i.`
- `STR_AI_TRANS_TARGET_LANGUAGE: Ch\u1ecdn ng\xf4n ng\u1eef d\u1ecbch`
- `STR_AI_TRANS_TARGET_LANG_EN: Ti\u1ebfng Anh`
- `STR_AI_TRANS_TARGET_LANG_KO: Ti\u1ebfng H\xe0n`
- `STR_AI_TRANS_TARGET_LANG_ZH_HANS: Ti\u1ebfng Trung`
- `STR_AI_TRANS_TRANSLATED_BY_ZALO_AI: D\u1ecbch b\u1edfi Zalo AI`
- `STR_AI_TRANS_TRANSLATE_MESSAGE: D\u1ecbch `
- `STR_AI_TRANS_TRANSLATING: \u0110ang d\u1ecbch`
- `STR_AI_TRANS_TRANSLATION_FAILED: C\xf3 l\u1ed7i x\u1ea3y ra.`
- `STR_AI_TRANS_URL_UNAVAILABLE: Url kh\xf4ng kh\u1ea3 d\u1ee5ng`
- `STR_AI_TRANS_ZCLOUD_UPSELL_AUTO_DESC: M\u1ed9t l\u1ea7n thi\u1ebft l\u1eadp, d\u1ecbch m\u1ecdi tin nh\u1eafn m\u1edbi`
- `STR_AI_TRANS_ZCLOUD_UPSELL_AUTO_TITLE: T\u1ef1 \u0111\u1ed9ng d\u1ecbch to\xe0n b\u1ed9 tr\xf2 chuy\u1ec7n`
- `STR_AI_TRANS_ZCLOUD_UPSELL_SUGGEST_DESC`
- `STR_AI_TRANS_ZCLOUD_UPSELL_SUGGEST_TITLE: G\u1ee3i \xfd b\u1ea3n d\u1ecbch khi so\u1ea1n tin`
- `STR_AI_TRANS_ZCLOUD_UPSELL_TITLE: Tr\u1ee3\xa0l\xfd\xa0d\u1ecbch\xa0Zalo\xa0AI, \u0111\u1eb7c\xa0quy\u1ec1n\xa0g\xf3i\xa0zCloud+`
- `STR_AI_TRANS_ZCLOUD_UPSELL_UPGRADE_BTN: D\u1ecbch tr\xf2 chuy\u1ec7n v\u1edbi zCloud+`
- `STR_OCR_COPY_SELECTED_CONTENT: Copy n\u1ed9i dung \u0111\xe3 ch\u1ecdn`
- `STR_RTF_TOOLTIP_WITH_HOTKEY: $0$ ($1$)`
- `STR_TOAST_RECEIVER_BLOCKS_STRANGERS`
- `STR_TURN_OFF_2FA_GUIDE_ON_MOBILE: \u0110\u1ec3 t\u1eaft, h\xe3y m\u1edf Zalo tr\xean \u0111i\u1ec7n tho\u1ea1i, v\xe0o $0$`
- `STR_TURN_OFF_2FA_GUIDE_PATH: C\xe0i \u0111\u1eb7t \u2192 T\xe0i kho\u1ea3n v\xe0 b\u1ea3o m\u1eadt`
- `STR_TURN_OFF_2FA_GUIDE_TITLE: Zalo PC ch\u01b0a h\u1ed7 tr\u1ee3 t\u1eaft b\u1ea3o m\u1eadt 2 l\u1edbp`

## Verification Checklist

- [ ] Verify `protocol.json` (`zalo_desktop_version`, `client_version`, `user_agent`) and `core/zalo-protocol/src/param_cipher.rs`.
- [ ] Verify `assets/wasm/libzproto_*.wasm` hashes & exports in `core/zalo-sync` and `core/zalo-backup`.
- [ ] Review new API endpoints and `STR_*` keys to port any high-value features to `core/zalo-client` & `zalo-linux/crates/zalo-gpui`.
- [ ] Keep `README.md` synchronized with any new user-facing features or configuration changes.
- [ ] Pass `cargo test --workspace -- --test-threads=1` across Linux, macOS, and Windows CI.
