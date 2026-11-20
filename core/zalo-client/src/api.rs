//! HTTP layer for the unauthenticated Zalo endpoints and QR login.

use crate::{Device, StoredSession};
use reqwest::cookie::{CookieStore, Jar};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, ORIGIN, REFERER};
use reqwest::{Client, StatusCode, Url};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use zalo_protocol::{
    aes_cbc_decrypt, aes_cbc_encrypt, api_type, base64_decode, base64_encode, client_version,
    sign_key_map, user_agent, ParamCipher,
};

/// Production authentication host.
pub const AUTH_BASE: &str = "https://wpa.zaloapp.com";

/// Production session host (getLoginInfo).
pub const WPA_BASE: &str = "https://wpa.chat.zalo.me";

/// A request or response failed before the chat layer is reached.
#[derive(Debug, Error)]
pub enum ClientError {
    /// The HTTP transport failed.
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    /// The response was not valid JSON.
    #[error("response JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    /// Zalo answered with a non-zero error_code.
    #[error("Zalo returned {code}: {message}")]
    Zalo {
        /// error_code from the response.
        code: i64,
        /// error_message from the response.
        message: String,
    },
    /// The response did not have the expected shape.
    #[error("unexpected response: {0}")]
    Unexpected(String),
    /// A required QR response field is missing.
    #[error("QR response has no {0}")]
    MissingField(&'static str),
    /// The login did not finish before qr_timeout.
    #[error("login timed out after {0} seconds")]
    Timeout(u64),
    /// ParamCipher or another protocol primitive failed.
    #[error("protocol error: {0}")]
    Protocol(#[from] zalo_protocol::CryptoError),
}

/// The QR ticket returned by /api/login/reqqr.
#[derive(Clone, Debug)]
pub struct QrTicket {
    /// Server token, for diagnostics.
    pub token_id: String,
    /// URL encoded in the QR image.
    pub qr_url: String,
    /// Long-poll URL that waits for the phone to scan.
    pub wait_scan: String,
    /// URL that turns a confirmed scan into a session.
    pub wait_confirm: String,
    /// Validity in seconds.
    pub timeout_seconds: u64,
    /// How many retries the server suggests.
    pub retry: u64,
    /// Raw response, useful while bringing up a new endpoint.
    pub raw: Value,
}

/// Account facts returned by getLoginInfo.
#[derive(Clone, Debug)]
pub struct LoginInfo {
    /// Account UIN (the dkey field).
    pub uin: String,
    /// Session data key, base64 encoded.
    pub zpw_enk: String,
    /// Phone number, when Zalo returns one.
    pub phone_number: Option<String>,
    /// Display name, when Zalo returns one.
    pub display_name: Option<String>,
    /// Logged-in user's own profile / My Cloud ID (`send2me_id`).
    pub send2me_id: Option<String>,
    /// Friend service host from zpw_service_map_v3.
    pub friend_host: Option<String>,
    /// Profile service host from zpw_service_map_v3 (getfriends lives here).
    pub profile_host: Option<String>,
    /// Conversation service host from zpw_service_map_v3.
    pub conversation_host: Option<String>,
    /// Direct chat service host from zpw_service_map_v3.
    pub chat_host: Option<String>,
    /// Group service host from zpw_service_map_v3.
    pub group_host: Option<String>,
    /// Realtime `wss://` endpoints from `zpw_ws`.
    pub ws_endpoints: Vec<String>,
    /// File/photo upload host from zpw_service_map_v3.
    pub file_host: Option<String>,
    /// Conversation label host from zpw_service_map_v3.
    pub label_host: Option<String>,
    /// Group cloud-message host (`group_cloud_message`) for older group history.
    pub group_cloud_host: Option<String>,
    /// Every service of `zpw_service_map_v3` with its hosts, keyed like the
    /// domain enum of Zalo PC (`chat`, `group`, `label`, …).
    pub service_hosts: std::collections::BTreeMap<String, Vec<String>>,
}

/// One sticker: what `/api/message/sticker` needs and the image to show.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StickerRef {
    /// Sticker id (`id` / `sticker_id`).
    pub id: i64,
    /// Pack id (`cateId` / `cate_id`).
    pub cate_id: i64,
    /// Sticker type (`type`, 7 for most packs).
    pub kind: i64,
    /// Image URL; [`StickerRef::image_url`] builds one when this is empty.
    pub url: String,
}

/// One GIF of the GIF store, as `/api/message/gif` sends it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GifRef {
    /// Animated GIF URL (`href`).
    pub url: String,
    /// Still or small preview (`thumb`).
    pub thumb: String,
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// Store id (`contentId` / `id`), sent back as is.
    pub content_id: Value,
}

impl StickerRef {
    /// Image to show: the reply's `stickerUrl`, or Zalo's 130 px PNG for `id`.
    pub fn image_url(&self) -> String {
        if self.url.starts_with("https://") {
            self.url.clone()
        } else {
            format!(
                "https://zalo-api.zadn.vn/api/emoticon/sticker/webpc?eid={}&size=130",
                self.id
            )
        }
    }

    /// One sticker object: search and suggestion replies use
    /// `sticker_id`/`cate_id`, pack lists `id`/`cateId`, `chat.sticker`
    /// content `id`/`catId`.
    pub fn from_value(value: &Value) -> Option<Self> {
        let num = |keys: &[&str]| {
            keys.iter().find_map(|k| {
                value.get(*k).and_then(|v| {
                    v.as_i64()
                        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
                })
            })
        };
        let id = num(&["sticker_id", "id", "stickerId"]).filter(|id| *id > 0)?;
        Some(Self {
            id,
            cate_id: num(&["cate_id", "cateId", "catId"]).unwrap_or(0),
            kind: num(&["type"]).unwrap_or(7),
            url: value
                .get("stickerUrl")
                .and_then(Value::as_str)
                .filter(|u| u.starts_with("https://"))
                .unwrap_or_default()
                .to_owned(),
        })
    }

    /// Stickers of a reply: `data` as a list, or `data.sugg_sticker`.
    pub fn list_from(value: &Value) -> Vec<Self> {
        let data = value.get("data").unwrap_or(value);
        let list = data
            .as_array()
            .or_else(|| data.get("sugg_sticker").and_then(Value::as_array));
        list.map(|items| items.iter().filter_map(Self::from_value).collect())
            .unwrap_or_default()
    }
}

impl GifRef {
    /// GIFs of a `giphy/*` reply (`data.data_list`, each with `normal`/`preview`).
    pub fn list_from(value: &Value) -> Vec<Self> {
        let data = value.get("data").unwrap_or(value);
        let Some(items) = data.get("data_list").and_then(Value::as_array) else {
            return Vec::new();
        };
        items
            .iter()
            .filter_map(|item| {
                let pick = |key: &str| item.get(key).filter(|v| v.is_object());
                let main = pick("normal")
                    .or_else(|| pick("hd"))
                    .or_else(|| pick("original"))?;
                let url = main.get("url").and_then(Value::as_str)?;
                if !url.starts_with("https://") {
                    return None;
                }
                let size = |key: &str| {
                    main.get(key)
                        .and_then(Value::as_u64)
                        .and_then(|n| u32::try_from(n).ok())
                        .unwrap_or(0)
                };
                let thumb = pick("preview")
                    .and_then(|p| p.get("url"))
                    .and_then(Value::as_str)
                    .filter(|u| u.starts_with("https://"))
                    .unwrap_or(url);
                Some(Self {
                    url: url.to_owned(),
                    thumb: thumb.to_owned(),
                    width: size("width"),
                    height: size("height"),
                    content_id: item.get("id").cloned().unwrap_or(Value::Null),
                })
            })
            .collect()
    }
}

/// A sticker pack of the Zalo sticker store.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StickerPack {
    /// Pack id (`cateId` of its stickers).
    pub id: i64,
    /// Pack name.
    pub name: String,
    /// Small icon for the pack strip.
    pub icon: String,
}

impl StickerPack {
    /// Visible packs of the `getAllCate` list: not `is_hidden`, not in
    /// `exCateIds` of the category reply, with permission.
    pub fn list_from(all: &Value, excluded: &[i64]) -> Vec<Self> {
        let Some(items) = all.as_array() else {
            return Vec::new();
        };
        items
            .iter()
            .filter(|c| c.get("is_hidden").and_then(Value::as_i64).unwrap_or(0) == 0)
            .filter(|c| c.get("permission").and_then(Value::as_i64).unwrap_or(1) != 0)
            .filter_map(|c| {
                let id = c.get("id").and_then(Value::as_i64)?;
                if excluded.contains(&id) {
                    return None;
                }
                let text = |k: &str| {
                    c.get(k)
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .trim()
                        .to_owned()
                };
                let icon = [text("iconUrl"), text("iconPreview")]
                    .into_iter()
                    .find(|u| u.starts_with("https://"))
                    .unwrap_or_default();
                Some(Self {
                    id,
                    name: text("name"),
                    icon,
                })
            })
            .collect()
    }
}

/// What to do with a friend request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FriendAnswer {
    /// Accept a received request.
    Accept,
    /// Decline a received request.
    Reject,
    /// Withdraw a request this account sent.
    Undo,
}

/// One conversation label (category tag) with the threads it is on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationLabel {
    /// Server label id.
    pub id: i64,
    /// Label text shown to the user.
    pub text: String,
    /// `#rrggbb` colour.
    pub color: String,
    /// Thread ids; groups carry the `g` prefix (`g123`).
    pub conversations: Vec<String>,
}

impl ConversationLabel {
    /// Labels from a decrypted `convlabel/get` reply, in the server's order.
    pub fn list_from(value: &Value) -> Vec<Self> {
        let root = value.get("data").filter(|v| v.is_object()).unwrap_or(value);
        let parsed;
        let data = match root.get("labelData") {
            Some(Value::String(text)) => {
                parsed = serde_json::from_str::<Value>(text).unwrap_or(Value::Null);
                &parsed
            }
            Some(other) => other,
            None => return Vec::new(),
        };
        let Some(items) = data.as_array() else {
            return Vec::new();
        };
        items
            .iter()
            .filter_map(|item| {
                let id = item.get("id").and_then(|v| {
                    v.as_i64()
                        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                })?;
                let text = item.get("text").and_then(Value::as_str)?.to_owned();
                let color = item
                    .get("color")
                    .and_then(Value::as_str)
                    .unwrap_or("#0068ff")
                    .to_owned();
                let conversations = item
                    .get("conversations")
                    .and_then(Value::as_array)
                    .map(|list| {
                        list.iter()
                            .filter_map(|c| {
                                c.as_str()
                                    .map(str::to_owned)
                                    .or_else(|| c.as_i64().map(|n| n.to_string()))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                Some(Self {
                    id,
                    text,
                    color,
                    conversations,
                })
            })
            .collect()
    }

    /// `version` from a `convlabel/get` reply.
    #[must_use]
    pub fn version_of(value: &Value) -> i64 {
        let root = value.get("data").filter(|v| v.is_object()).unwrap_or(value);
        root.get("version")
            .and_then(|v| {
                v.as_i64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
            .unwrap_or(0)
    }

    /// `labelData` JSON string for [`crate::ZaloApi::update_conversation_label`].
    #[must_use]
    pub fn to_label_data(labels: &[Self]) -> String {
        let items: Vec<Value> = labels
            .iter()
            .map(|label| {
                serde_json::json!({
                    "id": label.id,
                    "text": label.text,
                    "color": label.color,
                    "conversations": label.conversations,
                })
            })
            .collect();
        serde_json::to_string(&items).unwrap_or_else(|_| "[]".to_owned())
    }
}

/// One group member from `/api/group/getgi/v2`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupMember {
    /// User id.
    pub uid: String,
    /// Display name when the reply has one.
    pub name: String,
    /// Avatar URL when present.
    pub avatar: Option<String>,
}

impl GroupMember {
    /// Members from a decrypted `getgi/v2` reply.
    #[must_use]
    pub fn list_from(value: &Value) -> Vec<Self> {
        let root = value.get("data").filter(|v| v.is_object()).unwrap_or(value);
        let info = root
            .get("gridInfoMap")
            .and_then(Value::as_object)
            .and_then(|map| map.values().next())
            .unwrap_or(root);
        for key in ["currentMems", "members", "mems", "memInfos"] {
            if let Some(items) = info.get(key).and_then(Value::as_array) {
                let list: Vec<Self> = items.iter().filter_map(Self::from_value).collect();
                if !list.is_empty() {
                    return list;
                }
            }
        }
        info.get("memVerList")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        let raw = item.as_str()?;
                        let uid = raw.split('_').next().filter(|s| !s.is_empty())?;
                        Some(Self {
                            uid: uid.to_owned(),
                            name: String::new(),
                            avatar: None,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn from_value(value: &Value) -> Option<Self> {
        let uid = value
            .get("id")
            .or_else(|| value.get("uid"))
            .or_else(|| value.get("userId"))
            .and_then(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .or_else(|| v.as_i64().map(|n| n.to_string()))
            })
            .filter(|s| !s.is_empty())?;
        let name = value
            .get("dName")
            .or_else(|| value.get("displayName"))
            .or_else(|| value.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let avatar = value
            .get("avatar")
            .or_else(|| value.get("avt"))
            .and_then(Value::as_str)
            .filter(|u| u.starts_with("https://"))
            .map(str::to_owned);
        Some(Self { uid, name, avatar })
    }
}

/// Thread ids and version from `/api/archivedchat/list`.
#[must_use]
pub fn archived_ids_from(value: &Value) -> (Vec<String>, i64) {
    let root = value.get("data").filter(|v| v.is_object()).unwrap_or(value);
    let version = root
        .get("version")
        .and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(0);
    let from_str = |item: &Value| {
        item.as_str()
            .map(str::to_owned)
            .or_else(|| item.as_i64().map(|n| n.to_string()))
            .filter(|s| !s.is_empty())
    };
    if let Some(items) = root
        .get("ids")
        .or_else(|| root.get("list"))
        .or_else(|| root.get("data"))
        .and_then(Value::as_array)
    {
        let ids: Vec<String> = items.iter().filter_map(from_str).collect();
        if !ids.is_empty() {
            return (ids, version);
        }
    }
    if let Some(items) = root.get("items").and_then(Value::as_array) {
        let ids = items
            .iter()
            .filter_map(|item| {
                let id = from_str(item.get("id")?)?;
                let kind = item
                    .get("type")
                    .and_then(|v| {
                        v.as_i64()
                            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                    })
                    .unwrap_or(0);
                Some(if kind == 1 && !id.starts_with('g') {
                    format!("g{id}")
                } else {
                    id
                })
            })
            .collect();
        return (ids, version);
    }
    (Vec::new(), version)
}

/// Last decrypted body of a chunked upload, plus the id Zalo echoed.
struct ChunkedUpload {
    id: String,
    root: Value,
}

fn json_id(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(|v| {
        v.as_str()
            .map(str::to_owned)
            .or_else(|| v.as_i64().map(|n| n.to_string()))
    })
}

/// First playable image URL in a photo/video-thumb upload reply.
/// Order matches `getPhotoUrlFromResponseUpload`: raw, original, HD, normal, thumb.
fn upload_http_url(value: &Value) -> String {
    let mut nodes = Vec::new();
    nodes.push(value);
    if let Some(data) = value.get("data") {
        nodes.push(data);
        if let Some(inner) = data.get("data") {
            nodes.push(inner);
        }
    }
    for node in nodes {
        for key in ["rawUrl", "oriUrl", "hdUrl", "normalUrl", "thumbUrl", "url"] {
            if let Some(url) = node.get(key).and_then(Value::as_str) {
                if url.starts_with("https://") || url.starts_with("http://") {
                    return url.to_owned();
                }
            }
        }
    }
    String::new()
}

/// A photo uploaded with [`ZaloApi::upload_photo`], ready for [`ZaloApi::send_photo`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhotoUpload {
    /// Server photo id.
    pub photo_id: String,
    /// Client file id echoed by the server.
    pub client_file_id: String,
    /// Full-size URL.
    pub normal_url: String,
    /// HD URL (may equal `normal_url`).
    pub hd_url: String,
    /// Thumbnail URL.
    pub thumb_url: String,
    /// Uploaded byte count.
    pub size: usize,
}

impl PhotoUpload {
    fn from_value(value: &Value, size: usize) -> Result<Self, ClientError> {
        let root = value.get("data").filter(|v| v.is_object()).unwrap_or(value);
        let text = |key: &str| match root.get(key) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => String::new(),
        };
        let normal_url = text("normalUrl");
        let photo_id = text("photoId");
        if normal_url.is_empty() || photo_id.is_empty() {
            return Err(ClientError::Unexpected(format!(
                "photo upload has no normalUrl/photoId: {root}"
            )));
        }
        let hd_url = Some(text("hdUrl"))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| normal_url.clone());
        let thumb_url = Some(text("thumbUrl"))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| normal_url.clone());
        Ok(Self {
            photo_id,
            client_file_id: text("clientFileId"),
            normal_url,
            hd_url,
            thumb_url,
            size,
        })
    }
}

/// A file uploaded with [`ZaloApi::send_file`] (`mapFileResponse`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileUpload {
    /// Server file id.
    pub file_id: String,
    /// Download URL (`org` / `hd`).
    pub file_url: String,
    /// Byte count when the server sends `fileSize`.
    pub file_size: u64,
    /// Optional `secureKey`.
    pub secure_key: Option<String>,
}

impl FileUpload {
    fn from_value(value: &Value, size: u64) -> Result<Self, ClientError> {
        let root = value.get("data").filter(|v| v.is_object()).unwrap_or(value);
        let text = |key: &str| match root.get(key) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => String::new(),
        };
        let file_url = ["org", "hd", "fileUrl", "url"]
            .iter()
            .map(|k| text(k))
            .find(|s| s.starts_with("https://"))
            .unwrap_or_default();
        let file_id = text("fileId");
        if file_id.is_empty() && file_url.is_empty() {
            return Err(ClientError::Unexpected(format!(
                "file upload has no fileId/url: {root}"
            )));
        }
        let file_size = root
            .get("fileSize")
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_i64().and_then(|n| u64::try_from(n).ok()))
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
            .unwrap_or(size);
        let secure_key = Some(text("secureKey")).filter(|s| !s.is_empty());
        Ok(Self {
            file_id,
            file_url,
            file_size,
            secure_key,
        })
    }
}

/// Query body for `fetchImportant`. `newestId` wins; otherwise `lastId` (default `"-1"`).
pub fn important_payload(uid: &str, newest_id: Option<&str>, last_id: Option<&str>) -> Value {
    let mut payload = serde_json::json!({ "limit": 50, "uid": uid });
    if let Some(id) = newest_id.map(str::trim).filter(|s| !s.is_empty()) {
        payload["newestId"] = Value::String(id.to_owned());
    } else {
        let last = last_id
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("-1");
        payload["lastId"] = Value::String(last.to_owned());
    }
    payload
}

impl LoginInfo {
    fn from_value(value: Value) -> Result<Self, ClientError> {
        let data = value
            .get("data")
            .and_then(Value::as_object)
            .ok_or_else(|| ClientError::Unexpected("getLoginInfo has no data object".to_owned()))?;
        let uin = data
            .get("dkey")
            .and_then(Value::as_str)
            .ok_or_else(|| ClientError::Unexpected("getLoginInfo has no dkey".to_owned()))?
            .to_owned();
        let zpw_enk = data
            .get("zpw_enk")
            .and_then(Value::as_str)
            .ok_or_else(|| ClientError::Unexpected("getLoginInfo has no zpw_enk".to_owned()))?
            .to_owned();
        let phone_number = data
            .get("phone_number")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let display_name = data
            .get("send2me_name")
            .or_else(|| data.get("send2me_legacyName"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let send2me_id = data.get("send2me_id").and_then(|v| {
            v.as_str()
                .map(str::to_owned)
                .or_else(|| v.as_i64().map(|n| n.to_string()))
                .or_else(|| v.as_u64().map(|n| n.to_string()))
        });
        let map = data.get("zpw_service_map_v3");
        let friend_host = first_host(map, "friend");
        let profile_host = first_host(map, "profile");
        let conversation_host = first_host(map, "conversation");
        let chat_host = first_host(map, "chat");
        let group_host = first_host(map, "group");
        let ws_endpoints = crate::listener::ws_endpoints_from(&value);
        let file_host = first_host(map, "file");
        let label_host = first_host(map, "label");
        let group_cloud_host = first_host(map, "group_cloud_message");
        let service_hosts = map
            .and_then(Value::as_object)
            .map(|services| {
                services
                    .iter()
                    .map(|(name, hosts)| {
                        let list = hosts
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter_map(Value::as_str)
                                    .map(str::to_owned)
                                    .collect()
                            })
                            .unwrap_or_default();
                        (name.clone(), list)
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(Self {
            uin,
            zpw_enk,
            phone_number,
            display_name,
            send2me_id,
            friend_host,
            profile_host,
            conversation_host,
            chat_host,
            group_host,
            ws_endpoints,
            file_host,
            label_host,
            group_cloud_host,
            service_hosts,
        })
    }
}

/// A headless Zalo HTTP client.
pub struct ZaloApi {
    http: Client,
    jar: Arc<Jar>,
    pub(crate) device: Device,
    pub(crate) auth_base: String,
    zpw_sek: Option<String>,
    session_key: Option<Vec<u8>>,
    pub(crate) friend_host: Option<String>,
    pub(crate) profile_host: Option<String>,
    conversation_host: Option<String>,
    pub(crate) chat_host: Option<String>,
    pub(crate) group_host: Option<String>,
    ws_endpoints: Vec<String>,
    pub(crate) file_host: Option<String>,
    pub(crate) label_host: Option<String>,
    group_cloud_host: Option<String>,
    /// Every `zpw_service_map_v3` entry, for services without a field.
    service_hosts: std::collections::BTreeMap<String, Vec<String>>,
}

impl ZaloApi {
    /// Builds the client with a cookie jar and the Zalo web headers.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::Http`] when reqwest cannot build the client.
    pub fn new(device: Device) -> Result<Self, ClientError> {
        let jar = Arc::new(Jar::default());
        let mut headers = HeaderMap::new();
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/json, text/plain, */*"),
        );
        headers.insert(ORIGIN, HeaderValue::from_static("https://chat.zalo.me"));
        headers.insert(REFERER, HeaderValue::from_static("https://chat.zalo.me/"));
        let http = Client::builder()
            .cookie_provider(Arc::clone(&jar))
            .default_headers(headers)
            .user_agent(user_agent())
            .pool_idle_timeout(Duration::from_secs(30))
            .tcp_keepalive(Duration::from_secs(30))
            .timeout(Duration::from_secs(40))
            .build()?;
        Ok(Self {
            http,
            jar,
            device,
            auth_base: AUTH_BASE.to_owned(),
            zpw_sek: None,
            session_key: None,
            friend_host: None,
            profile_host: None,
            conversation_host: None,
            chat_host: None,
            group_host: None,
            ws_endpoints: Vec::new(),
            file_host: None,
            label_host: None,
            group_cloud_host: None,
            service_hosts: std::collections::BTreeMap::new(),
        })
    }

    /// Builds a client that resumes a saved session.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::Http`] when reqwest cannot build the client, or
    /// [`ClientError::Protocol`] when the saved `zpw_enk` is not valid base64.
    pub fn from_session(device: Device, session: &StoredSession) -> Result<Self, ClientError> {
        let mut api = Self::new(device)?;
        if let Some(sek) = session.zpw_sek() {
            api.set_zpw_sek(&sek);
        }
        if let Some(enk) = &session.zpw_enk {
            api.session_key = Some(base64_decode(enk)?);
        }
        Ok(api)
    }

    /// Stores `zpw_sek` and injects it as a cookie for Zalo's domains.
    ///
    /// The server accepts `zpw_sek` only as a cookie, not as an HTTP header,
    /// so the domain cookie is required on every host we call.
    pub fn set_zpw_sek(&mut self, sek: &str) {
        self.zpw_sek = Some(sek.to_owned());
        add_domain_cookie(
            &self.jar,
            "zpw_sek",
            sek,
            ".zaloapp.com",
            "https://zaloapp.com/",
        );
        add_domain_cookie(
            &self.jar,
            "zpw_sek",
            sek,
            ".zalo.me",
            "https://chat.zalo.me/",
        );
    }

    /// The device identity used by this client.
    pub fn device(&self) -> &Device {
        &self.device
    }

    /// The current account UIN, if known.
    pub fn zpw_sek(&self) -> Option<&str> {
        self.zpw_sek.as_deref()
    }

    /// Calls /api/login/getServerInfo.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, JSON, signature or Zalo errors.
    pub async fn get_server_info(&self) -> Result<Value, ClientError> {
        let mut params = self.base_params();
        params.insert("sp_mtn".to_owned(), "-1".to_owned());
        params.insert("bkt".to_owned(), "-1".to_owned());
        params.insert("signkey".to_owned(), sign_key_map("getserverinfo", &params));
        let url = format!("{}/api/login/getServerInfo", self.auth_base);
        let (status, body) = self.get_text(url, &params).await?;
        if !status.is_success() {
            return Err(ClientError::Unexpected(format!(
                "getServerInfo HTTP {status}: {body}"
            )));
        }
        let value: Value = serde_json::from_str(&body)?;
        ensure_success(&value)?;
        Ok(value)
    }

    /// Requests a QR code from /api/login/reqqr with self_g=1.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when the request fails or the response lacks
    /// the QR fields.
    pub async fn request_qr(&self) -> Result<QrTicket, ClientError> {
        let mut params = self.base_params();
        params.insert("language".to_owned(), "vi".to_owned());
        params.insert("client_time".to_owned(), unix_now().to_string());
        params.insert("logged_uids".to_owned(), String::new());
        params.insert("self_g".to_owned(), "1".to_owned());
        params.insert("qr_form".to_owned(), "0".to_owned());
        params.insert("signkey".to_owned(), sign_key_map("reqqr", &params));
        let url = format!("{}/api/login/reqqr", self.auth_base);
        let (status, body) = self.get_text(url, &params).await?;
        if !status.is_success() {
            return Err(ClientError::Unexpected(format!(
                "reqqr HTTP {status}: {body}"
            )));
        }
        let value: Value = serde_json::from_str(&body)?;
        ensure_success(&value)?;
        QrTicket::from_value(value)
    }

    /// Long-polls chk_wait_scan until the phone scans the code.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::Timeout`] after qr_timeout, or the transport or
    /// Zalo error that stopped the poll.
    pub async fn wait_for_scan(&self, ticket: &QrTicket) -> Result<Value, ClientError> {
        let deadline = Instant::now() + Duration::from_secs(ticket.timeout_seconds.max(30));
        let url = with_language(&ticket.wait_scan);
        loop {
            if Instant::now() >= deadline {
                return Err(ClientError::Timeout(ticket.timeout_seconds));
            }
            let response = self.http.get(&url).send().await?;
            let status = response.status();
            let body = response.text().await?;
            if status == StatusCode::REQUEST_TIMEOUT {
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
            if !status.is_success() {
                return Err(ClientError::Unexpected(format!(
                    "wait scan HTTP {status}: {body}"
                )));
            }
            let value: Value = serde_json::from_str(&body)?;
            if let Some((code, message)) = zalo_error(&value) {
                if code == 18012 {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
                if code != 0 {
                    return Err(ClientError::Zalo { code, message });
                }
            }
            return Ok(value);
        }
    }

    /// Long-polls chk_wait_cfm until the server hands out the session.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::Timeout`] after qr_timeout, or the transport or
    /// Zalo error that stopped the poll.
    pub async fn wait_for_confirm(&self, ticket: &QrTicket) -> Result<Value, ClientError> {
        let deadline = Instant::now() + Duration::from_secs(ticket.timeout_seconds.max(30));
        let url = with_language(&ticket.wait_confirm);
        loop {
            if Instant::now() >= deadline {
                return Err(ClientError::Timeout(ticket.timeout_seconds));
            }
            let response = self.http.get(&url).send().await?;
            let status = response.status();
            let body = response.text().await?;
            if status == StatusCode::REQUEST_TIMEOUT {
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
            if !status.is_success() {
                return Err(ClientError::Unexpected(format!(
                    "wait confirm HTTP {status}: {body}"
                )));
            }
            let value: Value = serde_json::from_str(&body)?;
            if let Some((code, message)) = zalo_error(&value) {
                if code == 18012 {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
                if code != 0 {
                    return Err(ClientError::Zalo { code, message });
                }
            }
            return Ok(value);
        }
    }

    /// Cookie headers currently stored for Zalo's domains.
    pub fn cookie_headers(&self) -> BTreeMap<String, String> {
        let mut headers = BTreeMap::new();
        for url in [
            "https://wpa.zaloapp.com/",
            "https://chat.zalo.me/",
            "https://qr-id.zaloapp.com/",
            "https://zaloapp.com/",
        ] {
            let Ok(parsed) = Url::parse(url) else {
                continue;
            };
            let Some(value) = self.jar.cookies(&parsed) else {
                continue;
            };
            if let Ok(text) = value.to_str() {
                if !text.is_empty() {
                    headers.insert(url.to_owned(), text.to_owned());
                }
            }
        }
        headers
    }

    /// Calls /api/login/getLoginInfo and stores the session data key.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, JSON, signature or Zalo errors.
    pub async fn get_login_info(&mut self) -> Result<LoginInfo, ClientError> {
        // Zalo PC sends getLoginInfo through ParamCipher v2 (`encryptParam`)
        // and decrypts `data` with the same key; the plain form is only its
        // fallback when the server answers 19000.
        let value = match self.get_login_info_encrypted().await {
            Ok(value) => value,
            Err(ClientError::Zalo { code: 19000, .. }) => self.get_login_info_plain().await?,
            Err(err) => return Err(err),
        };
        let info = LoginInfo::from_value(value)?;
        self.session_key = Some(base64_decode(&info.zpw_enk)?);
        self.apply_login_hosts(&info);
        Ok(info)
    }

    /// Applies the service hosts (`zpw_service_map_v3`) from a previously
    /// fetched [`LoginInfo`] without making another `/getLoginInfo` request.
    pub fn apply_login_hosts(&mut self, info: &LoginInfo) {
        self.friend_host = info.friend_host.clone();
        self.profile_host = info.profile_host.clone();
        self.conversation_host = info.conversation_host.clone();
        self.chat_host = info.chat_host.clone();
        self.group_host = info.group_host.clone();
        self.ws_endpoints = info.ws_endpoints.clone();
        self.file_host = info.file_host.clone();
        self.label_host = info.label_host.clone();
        self.group_cloud_host = info.group_cloud_host.clone();
        self.service_hosts = info.service_hosts.clone();
    }

    /// The getLoginInfo request as Zalo PC builds it (compact3178.js).
    fn login_info_request(
        &self,
        cipher: &ParamCipher,
    ) -> Result<BTreeMap<String, String>, ClientError> {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        let payload = serde_json::json!({
            "imei": self.device.imei,
            "computer_name": self.device.computer_name,
            "language": "vi",
            "ts": ts,
        });
        cipher
            .encrypt_params(&payload, "getlogininfo")
            .map_err(|e| ClientError::Unexpected(format!("getLoginInfo params: {e}")))
    }

    async fn get_login_info_encrypted(&self) -> Result<Value, ClientError> {
        let cipher =
            ParamCipher::random(&self.device.imei, self.device.first_launch_time, api_type())
                .map_err(|e| ClientError::Unexpected(format!("ParamCipher: {e}")))?;
        let params = self.login_info_request(&cipher)?;
        let url = format!("{WPA_BASE}/api/login/getLoginInfo");
        let (status, body) = self.get_text(url, &params).await?;
        if !status.is_success() {
            return Err(ClientError::Unexpected(format!(
                "getLoginInfo HTTP {status}: {body}"
            )));
        }
        let value: Value = serde_json::from_str(&body)?;
        ensure_success(&value)?;
        decrypt_login_reply(&cipher, value)
    }

    async fn get_login_info_plain(&self) -> Result<Value, ClientError> {
        let mut params = self.base_params();
        params.insert("signkey".to_owned(), sign_key_map("getlogininfo", &params));
        let url = format!("{WPA_BASE}/api/login/getLoginInfo");
        let (status, body) = self.get_text(url, &params).await?;
        if !status.is_success() {
            return Err(ClientError::Unexpected(format!(
                "getLoginInfo HTTP {status}: {body}"
            )));
        }
        let value: Value = serde_json::from_str(&body)?;
        ensure_success(&value)?;
        Ok(value)
    }

    /// Fetches the friend list and decrypts it with the session key.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when not logged in, or on transport, JSON,
    /// signature, Zalo or decryption errors.
    pub async fn get_friends(&self) -> Result<Value, ClientError> {
        self.get_friends_with(true).await
    }

    /// [`ZaloApi::get_friends`]; `init` adds Zalo PC's `init=1` flag of the
    /// first request after start.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn get_friends_with(&self, init: bool) -> Result<Value, ClientError> {
        let host = self
            .profile_host
            .as_deref()
            .or(self.friend_host.as_deref())
            .unwrap_or("https://tt-profile-wpa.zaloapp.com");
        let url = format!("{host}/api/social/friend/getfriends");
        // Without `params` the server returns only a handful of entries;
        // Zalo PC asks for everything (count 20000), invalid ones included.
        let mut params = self.common_params();
        let payload = serde_json::json!({
            "incInvalid": 1,
            "page": 1,
            "count": 20000,
            "avatar_size": 120,
            "actiontime": 0,
            "imei": self.device.imei,
        });
        params.insert("params".to_owned(), self.encode_params(&payload)?);
        if init {
            params.insert("init".to_owned(), "1".to_owned());
        }
        let (status, body) = self.get_text(url, &params).await?;
        if !status.is_success() {
            return Err(ClientError::Unexpected(format!(
                "getfriends HTTP {status}: {body}"
            )));
        }
        let value: Value = serde_json::from_str(&body)?;
        ensure_success(&value)?;
        self.decrypt_data(&value)
    }

    /// Friend list reconciliation (`/api/social/friend/verify`): the client
    /// sends the `uid_version` pairs it knows; Zalo answers with what to add,
    /// update or remove. An empty map asks for everything.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn verify_friends(
        &self,
        known: &[String],
        phonebook_version: i64,
    ) -> Result<Value, ClientError> {
        let payload = serde_json::json!({
            "friend_pversion_map": known,
            "phonebook_version": phonebook_version,
            "avatar_size": 120,
            "language": "vi",
            "imei": self.device.imei,
        });
        let base = self
            .profile_host
            .as_deref()
            .unwrap_or("https://tt-profile-wpa.chat.zalo.me")
            .to_owned();
        self.post_encrypted(&base, "/api/social/friend/verify", &payload)
            .await
    }

    /// Fetches the signed-in user's own profile (`/api/social/profile/me`) and
    /// decrypts it with the session key.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when not logged in, or on transport, JSON,
    /// Zalo or decryption errors.
    pub async fn get_me(&self) -> Result<Value, ClientError> {
        let host = self
            .profile_host
            .as_deref()
            .or(self.friend_host.as_deref())
            .unwrap_or("https://tt-profile-wpa.zaloapp.com");
        let payload = serde_json::json!({
            "avatar_size": 120,
            "imei": self.device.imei,
        });
        let mut params = self.common_params();
        params.insert("params".to_owned(), self.encode_params(&payload)?);
        let url = format!("{host}/api/social/profile/me");
        let (status, body) = self.get_text(url, &params).await?;
        if !status.is_success() {
            return Err(ClientError::Unexpected(format!(
                "get_me HTTP {status}: {body}"
            )));
        }
        let value: Value = serde_json::from_str(&body)?;
        ensure_success(&value)?;
        self.decrypt_data(&value)
    }

    /// Own profile plus the zBusiness card (`/api/social/profile/me-v2`).
    ///
    /// Electron `getProfileMeV2` sends the same payload as [`Self::get_me`].
    /// The decrypted body is `{ profile, biz }`: `bizPkg` lives on `profile`
    /// and the description lives on the sibling `biz` object.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when not logged in, or on transport, JSON,
    /// Zalo or decryption errors.
    pub async fn get_me_v2(&self) -> Result<Value, ClientError> {
        let host = self
            .profile_host
            .as_deref()
            .or(self.friend_host.as_deref())
            .unwrap_or("https://tt-profile-wpa.zaloapp.com");
        let payload = serde_json::json!({
            "avatar_size": 120,
            "imei": self.device.imei,
        });
        self.get_encrypted(host, "/api/social/profile/me-v2", &payload)
            .await
    }

    /// zBusiness card for one user (`/api/social/friend/get-bizacc`).
    ///
    /// Electron calls this when `bizPkg.pkgId` is set and merges the reply
    /// onto `bizInfo`.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when not logged in, or on transport, JSON,
    /// Zalo or decryption errors.
    pub async fn get_biz_card(&self, fid: &str) -> Result<Value, ClientError> {
        let host = self
            .profile_host
            .as_deref()
            .or(self.friend_host.as_deref())
            .unwrap_or("https://tt-profile-wpa.zaloapp.com");
        let payload = serde_json::json!({ "fid": fid });
        self.post_encrypted(host, "/api/social/friend/get-bizacc", &payload)
            .await
    }

    /// Friend cards including `bizPkg` (`/api/social/friend/getprofiles/v2`).
    ///
    /// `friend_pversion_map` entries are `{uid}_0`, matching Electron
    /// `getFriendProfilev2`.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when not logged in, or on transport, JSON,
    /// Zalo or decryption errors.
    pub async fn get_profiles_v2(&self, uids: &[&str]) -> Result<Value, ClientError> {
        let host = self
            .profile_host
            .as_deref()
            .or(self.friend_host.as_deref())
            .unwrap_or("https://tt-profile-wpa.zaloapp.com");
        let versions: Vec<String> = uids.iter().map(|uid| format!("{uid}_0")).collect();
        let payload = serde_json::json!({
            "phonebook_version": 0,
            "friend_pversion_map": versions,
            "avatar_size": 120,
            "language": "vi",
            "show_online_status": 1,
            "imei": self.device.imei,
        });
        self.post_encrypted(host, "/api/social/friend/getprofiles/v2", &payload)
            .await
    }

    /// Fetches the latest message per conversation and decrypts the reply.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when not logged in, or on transport, JSON,
    /// signature, Zalo or decryption errors.
    pub async fn get_last_messages(&self) -> Result<Value, ClientError> {
        self.get_last_messages_for(empty_last_msgs_map()).await
    }

    /// Same call as [`Self::get_last_messages`] with an explicit
    /// `threadIdLocalMsgId` string. Electron uses `{}` on a fresh profile and
    /// `{uid_0: msgId, groupId_1: msgId}` once it knows threads.
    pub async fn get_last_messages_for(&self, thread_map: &str) -> Result<Value, ClientError> {
        let host = self
            .conversation_host
            .as_deref()
            .unwrap_or("https://tt-convers-wpa.api.zaloapp.com");
        let payload = serde_json::json!({
            "threadIdLocalMsgId": thread_map,
            "imei": self.device.imei,
        });
        let mut params = self.common_params();
        params.insert("params".to_owned(), self.encode_params(&payload)?);
        let url = format!("{host}/api/preloadconvers/get-last-msgs");
        let (status, body) = self.get_text(url, &params).await?;
        if !status.is_success() {
            return Err(ClientError::Unexpected(format!(
                "get-last-msgs HTTP {status}: {body}"
            )));
        }
        let value: Value = serde_json::from_str(&body)?;
        ensure_success(&value)?;
        self.decrypt_data(&value)
    }

    /// GET `{host}{path}` with `params` = encrypted `payload`, decrypted reply.
    pub(crate) async fn get_encrypted(
        &self,
        host: &str,
        path: &str,
        payload: &Value,
    ) -> Result<Value, ClientError> {
        let mut params = self.common_params();
        params.insert("params".to_owned(), self.encode_params(payload)?);
        let (status, body) = self.get_text(format!("{host}{path}"), &params).await?;
        if !status.is_success() {
            return Err(ClientError::Unexpected(format!(
                "{path} HTTP {status}: {body}"
            )));
        }
        let value: Value = serde_json::from_str(&body)?;
        ensure_success(&value)?;
        self.decrypt_data(&value)
    }

    /// POST `{host}{path}` with form `params` = encrypted `payload`.
    pub(crate) async fn post_encrypted(
        &self,
        host: &str,
        path: &str,
        payload: &Value,
    ) -> Result<Value, ClientError> {
        let query = self.common_params();
        let mut form = BTreeMap::new();
        form.insert("params".to_owned(), self.encode_params(payload)?);
        let (status, body) = self
            .post_form(format!("{host}{path}"), &query, &form)
            .await?;
        if !status.is_success() {
            return Err(ClientError::Unexpected(format!(
                "{path} HTTP {status}: {body}"
            )));
        }
        let value: Value = serde_json::from_str(&body)?;
        ensure_success(&value)?;
        self.decrypt_data(&value)
    }

    /// Urgent and important messages (`/api/message/fetchImportant`, cmd 12114).
    ///
    /// Electron sends `{limit:50, uid}` plus `newestId` or `lastId`. The first
    /// page uses `lastId = "-1"`.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn fetch_important(
        &self,
        uid: &str,
        newest_id: Option<&str>,
        last_id: Option<&str>,
    ) -> Result<Value, ClientError> {
        let payload = important_payload(uid, newest_id, last_id);
        let host = self
            .chat_host
            .clone()
            .unwrap_or_else(|| "https://tt-chat-wpa.chat.zalo.me".to_owned());
        self.get_encrypted(&host, "/api/message/fetchImportant", &payload)
            .await
    }

    /// First host of a `zpw_service_map_v3` service, or `fallback`.
    pub(crate) fn service_base<'a>(&'a self, name: &str, fallback: &'a str) -> &'a str {
        self.service_hosts
            .get(name)
            .and_then(|hosts| hosts.first())
            .map_or(fallback, String::as_str)
    }

    /// POST `{host}{path}` with the encrypted `params` in the query and no
    /// body, as Zalo PC's autoreply calls do.
    async fn post_encrypted_query(
        &self,
        host: &str,
        path: &str,
        payload: &Value,
    ) -> Result<Value, ClientError> {
        let mut query = self.common_params();
        query.insert("params".to_owned(), self.encode_params(payload)?);
        let (status, body) = self
            .post_form(format!("{host}{path}"), &query, &BTreeMap::new())
            .await?;
        if !status.is_success() {
            return Err(ClientError::Unexpected(format!(
                "{path} HTTP {status}: {body}"
            )));
        }
        let value: Value = serde_json::from_str(&body)?;
        ensure_success(&value)?;
        self.decrypt_data(&value)
    }

    pub(crate) fn conversation_base(&self) -> &str {
        self.conversation_host
            .as_deref()
            .unwrap_or("https://tt-convers-wpa.chat.zalo.me")
    }

    /// Hidden conversations and the md5 of their PIN
    /// (`/api/hiddenconvers/get-all`): `{ pin, threads: [{ thread_id, is_group }] }`.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn get_hidden_chats(&self) -> Result<Value, ClientError> {
        let payload = serde_json::json!({ "imei": self.device.imei });
        self.get_encrypted(
            self.conversation_base(),
            "/api/hiddenconvers/get-all",
            &payload,
        )
        .await
    }

    /// Hides `add` and unhides `del` (`/api/hiddenconvers/add-remove`). Each
    /// item is `{ "thread_id": "...", "is_group": 0|1 }`.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn update_hidden_chats(
        &self,
        add: &[Value],
        del: &[Value],
    ) -> Result<Value, ClientError> {
        let payload = serde_json::json!({
            "del_threads": Value::from(del.to_vec()).to_string(),
            "add_threads": Value::from(add.to_vec()).to_string(),
            "imei": self.device.imei,
        });
        self.post_encrypted(
            self.conversation_base(),
            "/api/hiddenconvers/add-remove",
            &payload,
        )
        .await
    }

    /// Sets the hidden-chat PIN (`/api/hiddenconvers/update-pin`). Zalo PC
    /// sends the md5 hex of the four digits.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn update_hidden_pin(&self, pin: &str) -> Result<Value, ClientError> {
        let payload = serde_json::json!({
            "new_pin": zalo_protocol::md5_hex(pin.as_bytes()),
            "imei": self.device.imei,
        });
        self.get_encrypted(
            self.conversation_base(),
            "/api/hiddenconvers/update-pin",
            &payload,
        )
        .await
    }

    /// Clears the hidden-chat PIN and unhides everything (`/api/hiddenconvers/reset`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn reset_hidden_chats(&self) -> Result<Value, ClientError> {
        let base = self.conversation_base().to_owned();
        let (status, body) = self
            .get_text(
                format!("{base}/api/hiddenconvers/reset"),
                &self.common_params(),
            )
            .await?;
        if !status.is_success() {
            return Err(ClientError::Unexpected(format!(
                "hiddenconvers/reset HTTP {status}: {body}"
            )));
        }
        let value: Value = serde_json::from_str(&body)?;
        ensure_success(&value)?;
        self.decrypt_data(&value)
    }

    fn autoreply_base(&self) -> &str {
        self.service_base("auto_reply", "https://tt-autoreply-wpa.chat.zalo.me")
    }

    /// Auto-reply rules (`/api/autoreply/list`): `item` is a list of
    /// `{ id, enable, content, startTime, endTime, recurrence, scope, uids }`.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn get_auto_replies(&self) -> Result<Value, ClientError> {
        let payload = serde_json::json!({ "version": 0, "cliLang": "vi" });
        self.get_encrypted(self.autoreply_base(), "/api/autoreply/list", &payload)
            .await
    }

    /// Creates (`id` = `None`, `/api/autoreply/create`) or updates
    /// (`/api/autoreply/update`) an auto-reply. Times are unix milliseconds;
    /// `scope` 0 = everyone; `recurrence` as Zalo PC sends it.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    #[allow(clippy::too_many_arguments)]
    pub async fn save_auto_reply(
        &self,
        id: Option<i64>,
        enable: bool,
        content: &str,
        start_ms: i64,
        end_ms: i64,
        recurrence: &[String],
        scope: i64,
        uids: &[String],
    ) -> Result<Value, ClientError> {
        let mut payload = serde_json::json!({
            "cliLang": "vi",
            "enable": enable,
            "content": content,
            "startTime": start_ms,
            "endTime": end_ms,
            "recurrence": recurrence,
            "scope": scope,
            "uids": uids,
        });
        let path = match id {
            Some(id) => {
                payload["id"] = Value::from(id);
                "/api/autoreply/update"
            }
            None => "/api/autoreply/create",
        };
        self.post_encrypted_query(self.autoreply_base(), path, &payload)
            .await
    }

    /// Deletes an auto-reply (`/api/autoreply/delete`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn delete_auto_reply(&self, id: i64) -> Result<Value, ClientError> {
        let payload = serde_json::json!({ "cliLang": "vi", "id": id });
        self.post_encrypted_query(self.autoreply_base(), "/api/autoreply/delete", &payload)
            .await
    }

    /// Updates name, gender and birthday (`/api/social/profile/update`), like
    /// Zalo PC's profile editor: `profile` and `biz` are JSON strings.
    /// `dob` is `YYYY-MM-DD`.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn update_profile(
        &self,
        name: &str,
        gender: i64,
        dob: &str,
    ) -> Result<Value, ClientError> {
        let payload = serde_json::json!({
            "profile": serde_json::json!({ "name": name, "gender": gender, "dob": dob }).to_string(),
            "biz": "{}",
            "language": "vi",
        });
        let base = self
            .profile_host
            .as_deref()
            .unwrap_or("https://tt-profile-wpa.chat.zalo.me")
            .to_owned();
        self.post_encrypted(&base, "/api/social/profile/update", &payload)
            .await
    }

    pub(crate) fn friend_base(&self) -> &str {
        self.friend_host
            .as_deref()
            .unwrap_or_else(|| self.service_base("friend", "https://tt-friend-wpa.chat.zalo.me"))
    }

    /// Looks a person up by phone number (`/api/friend/profile/get`), as the
    /// "Add friend" dialog does. The reply has `uid`, `zalo_name`,
    /// `display_name`, `avatar`.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo (e.g. no account for that
    /// number) or decryption errors.
    pub async fn find_user_by_phone(&self, phone: &str) -> Result<Value, ClientError> {
        let payload = serde_json::json!({
            "phone": phone,
            "avatar_size": 240,
            "language": "vi",
            "imei": self.device.imei,
        });
        self.get_encrypted(self.friend_base(), "/api/friend/profile/get", &payload)
            .await
    }

    /// Sends a friend request with a greeting (`/api/friend/sendreq`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn send_friend_request(
        &self,
        to_uid: &str,
        message: &str,
    ) -> Result<Value, ClientError> {
        let payload = serde_json::json!({
            "toid": to_uid,
            "msg": message,
            "reqsrc": 30,
            "imei": self.device.imei,
            "language": "vi",
            "srcParams": serde_json::json!({ "uidTo": to_uid }).to_string(),
        });
        self.post_encrypted(self.friend_base(), "/api/friend/sendreq", &payload)
            .await
    }

    /// One page of the people whose messages are blocked
    /// (`/api/friend/blocklistv2`, block list). The reply has
    /// `profiles` (with `userId`, `displayName`, `zaloName`, `avatar`),
    /// `has_more` and `version`. Pages start at 1.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn get_block_list(&self, page: u32, count: u32) -> Result<Value, ClientError> {
        let payload = serde_json::json!({ "page": page, "count": count, "avatar": 75 });
        self.get_encrypted(self.friend_base(), "/api/friend/blocklistv2", &payload)
            .await
    }

    /// Blocks (`/api/friend/block`) or unblocks (`/api/friend/unblock`)
    /// messages from `uid`, like Zalo PC's `setBlockFriend`.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn set_block(&self, uid: &str, block: bool) -> Result<Value, ClientError> {
        let payload = serde_json::json!({ "fid": uid, "imei": self.device.imei });
        let path = if block {
            "/api/friend/block"
        } else {
            "/api/friend/unblock"
        };
        self.post_encrypted(self.friend_base(), path, &payload)
            .await
    }

    /// Friend suggestions and received requests
    /// (`/api/friend/recommendsv2/list`): `recommItems`, each with
    /// `dataInfo` (userId, displayName, avatar, recommType, recommSrc…).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn get_recommended_friends(&self) -> Result<Value, ClientError> {
        let payload = serde_json::json!({ "imei": self.device.imei });
        self.get_encrypted(
            self.friend_base(),
            "/api/friend/recommendsv2/list",
            &payload,
        )
        .await
    }

    /// Friend requests this account sent and that are still pending
    /// (`/api/friend/requested/list`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn get_sent_requests(&self) -> Result<Value, ClientError> {
        let payload = serde_json::json!({ "imei": self.device.imei });
        self.get_encrypted(self.friend_base(), "/api/friend/requested/list", &payload)
            .await
    }

    /// Accepts (`/api/friend/accept`) or rejects (`/api/friend/reject`) a
    /// received friend request, or withdraws a sent one (`/api/friend/undo`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn answer_friend_request(
        &self,
        uid: &str,
        action: FriendAnswer,
    ) -> Result<Value, ClientError> {
        let (path, payload) = match action {
            FriendAnswer::Accept => (
                "/api/friend/accept",
                serde_json::json!({ "fid": uid, "language": "vi" }),
            ),
            FriendAnswer::Reject => ("/api/friend/reject", serde_json::json!({ "fid": uid })),
            FriendAnswer::Undo => ("/api/friend/undo", serde_json::json!({ "fid": uid })),
        };
        self.post_encrypted(self.friend_base(), path, &payload)
            .await
    }

    /// Creates a group with `members` (user ids) (`/api/group/create/v2`).
    /// An empty `name` lets Zalo name it after the members. The reply carries
    /// `groupId`.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn create_group(&self, name: &str, members: &[String]) -> Result<Value, ClientError> {
        let host = self
            .group_host
            .as_deref()
            .unwrap_or("https://tt-group-wpa.chat.zalo.me");
        let client_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let payload = serde_json::json!({
            "clientId": client_id,
            "gname": name,
            "gdesc": null,
            "members": members,
            "memberTypes": members.iter().map(|_| -1).collect::<Vec<_>>(),
            "nameChanged": i32::from(!name.trim().is_empty()),
            "createLink": 1,
            "clientLang": "vi",
            "imei": self.device.imei,
            "zsource": 601,
        });
        self.get_encrypted(host, "/api/group/create/v2", &payload)
            .await
    }

    /// Latest messages of a group (`/api/group/history`, `getHistoryMessage`).
    /// `group_id` may carry the `g_` prefix. The reply holds `groupMsgs`.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn get_group_history(
        &self,
        group_id: &str,
        count: u32,
    ) -> Result<Value, ClientError> {
        let host = self
            .group_host
            .clone()
            .unwrap_or_else(|| "https://tt-group-wpa.chat.zalo.me".to_owned());
        let grid = group_id.trim_start_matches("g_");
        let payload = serde_json::json!({ "grid": grid, "count": count });
        self.get_encrypted(&host, "/api/group/history", &payload)
            .await
    }

    /// Group messages older than `before_msg_id` from the group message cloud
    /// (`/api/cm/getoldv2`, `getCM`), newest first in pages of `count`.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors (groups
    /// without cloud history answer with a Zalo error).
    pub async fn get_group_older(
        &self,
        group_id: &str,
        before_msg_id: &str,
        count: u32,
    ) -> Result<Value, ClientError> {
        self.group_cloud(group_id, before_msg_id, count, "/api/cm/getoldv2")
            .await
    }

    /// Newest group messages from the group message cloud (`/api/cm/getrecentv2`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn get_group_recent(&self, group_id: &str, count: u32) -> Result<Value, ClientError> {
        self.group_cloud(group_id, "0", count, "/api/cm/getrecentv2")
            .await
    }

    async fn group_cloud(
        &self,
        group_id: &str,
        before_msg_id: &str,
        count: u32,
        path: &str,
    ) -> Result<Value, ClientError> {
        let host = self
            .group_cloud_host
            .clone()
            .ok_or_else(|| ClientError::Unexpected("no group_cloud_message host".to_owned()))?;
        // Zalo compares ids as numbers; "newest" is a very large id.
        let global: u64 = before_msg_id
            .parse()
            .ok()
            .filter(|id| *id > 0)
            .unwrap_or(10_000_000_000_000_000);
        let payload = serde_json::json!({
            "groupId": group_id.trim_start_matches("g_"),
            "globalMsgId": global,
            "count": count,
            "msgIds": [],
            "imei": self.device.imei,
            "src": 1,
        });
        self.get_encrypted(&host, path, &payload).await
    }

    /// Tells Zalo the newest message of a conversation was read here
    /// (`/api/message/seenv2` or `/api/group/seenv2`), which also clears the
    /// unread count on the user's other devices. `items` are Zalo's
    /// `{cmi, gmi, si, di, mt, st, at, cmd, ts}` records.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn send_seen(&self, thread_id: &str, items: &[Value]) -> Result<Value, ClientError> {
        let group = thread_id.starts_with("g_");
        let (host, path, info) = if group {
            (
                self.group_host
                    .clone()
                    .unwrap_or_else(|| "https://tt-group-wpa.chat.zalo.me".to_owned()),
                "/api/group/seenv2",
                serde_json::json!({ "data": items, "grid": thread_id.trim_start_matches("g_") }),
            )
        } else {
            (
                self.chat_host
                    .clone()
                    .unwrap_or_else(|| "https://tt-chat2-wpa.chat.zalo.me".to_owned()),
                "/api/message/seenv2",
                serde_json::json!({ "data": items, "senderId": thread_id }),
            )
        };
        let payload = serde_json::json!({
            "msgInfos": info.to_string(),
            "imei": self.device.imei,
        });
        self.post_encrypted(&host, path, &payload).await
    }

    /// Account settings (`/api/setting/me`): privacy keys such as
    /// `show_online_status`, `display_seen_status`, `receive_message`,
    /// `accept_stranger_call`, `view_birthday`.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn get_settings(&self) -> Result<Value, ClientError> {
        let url = format!("{}/api/setting/me", self.auth_base);
        let params = self.common_params();
        let (status, body) = self.get_text(url, &params).await?;
        if !status.is_success() {
            return Err(ClientError::Unexpected(format!(
                "setting/me HTTP {status}: {body}"
            )));
        }
        let value: Value = serde_json::from_str(&body)?;
        ensure_success(&value)?;
        self.decrypt_data(&value)
    }

    /// Changes one account setting (`/api/setting/update`), e.g.
    /// `("show_online_status", 0)`.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn update_setting(&self, key: &str, value: i64) -> Result<Value, ClientError> {
        let mut payload = serde_json::Map::new();
        payload.insert(key.to_owned(), Value::from(value));
        let base = self.auth_base.clone();
        self.get_encrypted(&base, "/api/setting/update", &Value::Object(payload))
            .await
    }

    /// Fetches the conversation labels (category tags) with
    /// `/api/convlabel/get`. The reply's `labelData` is a JSON string; see
    /// [`ConversationLabel::list_from`].
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when not logged in, or on transport, JSON,
    /// Zalo or decryption errors.
    pub async fn get_labels(&self) -> Result<Value, ClientError> {
        let host = self
            .label_host
            .as_deref()
            .unwrap_or("https://label-wpa.chat.zalo.me");
        let payload = serde_json::json!({ "imei": self.device.imei });
        let mut params = self.common_params();
        params.insert("params".to_owned(), self.encode_params(&payload)?);
        let url = format!("{host}/api/convlabel/get");
        let (status, body) = self.get_text(url, &params).await?;
        if !status.is_success() {
            return Err(ClientError::Unexpected(format!(
                "convlabel/get HTTP {status}: {body}"
            )));
        }
        let value: Value = serde_json::from_str(&body)?;
        ensure_success(&value)?;
        self.decrypt_data(&value)
    }

    /// Fetches the list of joined group IDs and versions (`/api/group/getlg/v4`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when not logged in, or on transport, JSON,
    /// Zalo or decryption errors.
    pub async fn get_group_list(&self) -> Result<Value, ClientError> {
        let host = self
            .group_host
            .as_deref()
            .unwrap_or("https://tt-group-wpa.chat.zalo.me");
        let url = format!("{host}/api/group/getlg/v4");
        let params = self.common_params();
        let (status, body) = self.get_text(url, &params).await?;
        if !status.is_success() {
            return Err(ClientError::Unexpected(format!(
                "getlg/v4 HTTP {status}: {body}"
            )));
        }
        let value: Value = serde_json::from_str(&body)?;
        ensure_success(&value)?;
        self.decrypt_data(&value)
    }

    /// Groups the server marks as end-to-end encrypted (`/api/group/getlg/e2ee`, cmd 12146).
    ///
    /// Electron sends common params only. The decrypted body is a list of group ids.
    /// This client does not encrypt message bodies.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when not logged in, or on transport, JSON,
    /// Zalo or decryption errors.
    pub async fn get_e2ee_group_list(&self) -> Result<Value, ClientError> {
        let host = self
            .group_host
            .as_deref()
            .unwrap_or("https://tt-group-wpa.chat.zalo.me");
        let url = format!("{host}/api/group/getlg/e2ee");
        let params = self.common_params();
        let (status, body) = self.get_text(url, &params).await?;
        if !status.is_success() {
            return Err(ClientError::Unexpected(format!(
                "getlg/e2ee HTTP {status}: {body}"
            )));
        }
        let value: Value = serde_json::from_str(&body)?;
        ensure_success(&value)?;
        self.decrypt_data(&value)
    }

    /// Fetches detailed group profiles for a slice of group IDs (`/api/group/getmg-v2`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when not logged in, or on transport, JSON,
    /// Zalo or decryption errors.
    pub async fn get_multi_group_info(&self, group_ids: &[String]) -> Result<Value, ClientError> {
        let host = self
            .group_host
            .as_deref()
            .unwrap_or("https://tt-group-wpa.chat.zalo.me");
        let mut ver_map = serde_json::Map::new();
        for gid in group_ids {
            let clean = gid.strip_prefix("g_").unwrap_or(gid);
            if !clean.is_empty() {
                ver_map.insert(clean.to_owned(), Value::from(0));
            }
        }
        let payload = serde_json::json!({
            "gridVerMap": serde_json::to_string(&ver_map)?,
        });
        let url = format!("{host}/api/group/getmg-v2");
        let query = self.common_params();
        let mut form = BTreeMap::new();
        form.insert("params".to_owned(), self.encode_params(&payload)?);
        let (status, body) = self.post_form(url, &query, &form).await?;
        if !status.is_success() {
            return Err(ClientError::Unexpected(format!(
                "getmg-v2 HTTP {status}: {body}"
            )));
        }
        let value: Value = serde_json::from_str(&body)?;
        ensure_success(&value)?;
        self.decrypt_data(&value)
    }

    /// Sends a plain-text direct message (`/api/message/sms`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when not logged in, or on transport, JSON,
    /// Zalo or decryption errors.
    pub async fn send_message(
        &self,
        to_uid: &str,
        message: &str,
        client_id: u64,
    ) -> Result<Value, ClientError> {
        let host = self
            .chat_host
            .as_deref()
            .unwrap_or("https://tt-chat2-wpa.chat.zalo.me");
        let payload = serde_json::json!({
            "message": message,
            "clientId": client_id,
            "imei": self.device.imei,
            "toid": to_uid,
        });
        let url = format!("{host}/api/message/sms");
        let query = self.common_params();
        let mut form = BTreeMap::new();
        form.insert("params".to_owned(), self.encode_params(&payload)?);
        let (status, body) = self.post_form(url, &query, &form).await?;
        if !status.is_success() {
            return Err(ClientError::Unexpected(format!(
                "send_message HTTP {status}: {body}"
            )));
        }
        let value: Value = serde_json::from_str(&body)?;
        ensure_success(&value)?;
        self.decrypt_data(&value)
    }

    /// Sends a plain-text group message (`/api/group/sendmsg`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when not logged in, or on transport, JSON,
    /// Zalo or decryption errors.
    pub async fn send_group_message(
        &self,
        group_id: &str,
        message: &str,
        client_id: u64,
    ) -> Result<Value, ClientError> {
        let host = self
            .group_host
            .as_deref()
            .unwrap_or("https://tt-group-wpa.chat.zalo.me");
        let grid = group_id.strip_prefix("g_").unwrap_or(group_id);
        let payload = serde_json::json!({
            "message": message,
            "clientId": client_id,
            "imei": self.device.imei,
            "grid": grid,
            "visibility": 0,
        });
        let url = format!("{host}/api/group/sendmsg");
        let query = self.common_params();
        let mut form = BTreeMap::new();
        form.insert("params".to_owned(), self.encode_params(&payload)?);
        let (status, body) = self.post_form(url, &query, &form).await?;
        if !status.is_success() {
            return Err(ClientError::Unexpected(format!(
                "send_group_message HTTP {status}: {body}"
            )));
        }
        let value: Value = serde_json::from_str(&body)?;
        ensure_success(&value)?;
        self.decrypt_data(&value)
    }

    /// Sticker service host (`sticker` in `zpw_service_map_v3`).
    pub(crate) fn sticker_base(&self) -> &str {
        self.service_base("sticker", "https://sticker-wpa.chat.zalo.me")
    }

    /// GET a sticker-service path whose `params` is the AES of `plain`
    /// as-is (Zalo PC sends `""` for the category list, not JSON).
    async fn get_sticker_raw(&self, path: &str, plain: &str) -> Result<Value, ClientError> {
        let key = self
            .session_key
            .as_ref()
            .ok_or_else(|| ClientError::Unexpected("no session data key".to_owned()))?;
        let mut params = self.common_params();
        params.insert(
            "params".to_owned(),
            base64_encode(&aes_cbc_encrypt(key, plain.as_bytes())?),
        );
        let host = self.sticker_base().to_owned();
        let (status, body) = self.get_text(format!("{host}{path}"), &params).await?;
        if !status.is_success() {
            return Err(ClientError::Unexpected(format!(
                "{path} HTTP {status}: {body}"
            )));
        }
        let value: Value = serde_json::from_str(&body)?;
        ensure_success(&value)?;
        self.decrypt_data(&value)
    }

    /// The account's sticker packs (`/api/message/sticker/category/list/v2`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn get_sticker_categories(&self) -> Result<Value, ClientError> {
        self.get_sticker_raw("/api/message/sticker/category/list/v2", "")
            .await
    }

    /// The sticker store: the packs listed at the `dataUrl` that
    /// [`ZaloApi::get_sticker_categories`] points to (a gzipped JSON list),
    /// without hidden and excluded ones.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo, decryption or JSON errors.
    pub async fn get_sticker_store(&self) -> Result<Vec<StickerPack>, ClientError> {
        let reply = self.get_sticker_categories().await?;
        let data = reply.get("data").unwrap_or(&reply);
        let excluded: Vec<i64> = data
            .get("exCateIds")
            .and_then(Value::as_array)
            .map(|ids| ids.iter().filter_map(Value::as_i64).collect())
            .unwrap_or_default();
        let url = data
            .get("dataUrl")
            .and_then(Value::as_str)
            .filter(|u| u.contains("getAllCate"))
            .map(|u| u.replacen("http://", "https://", 1))
            .ok_or_else(|| ClientError::Unexpected("sticker store has no dataUrl".to_owned()))?;
        let raw = self.fetch_limited(&url, 8_000_000).await?;
        let bytes = if raw.starts_with(&[0x1f, 0x8b]) {
            use std::io::Read as _;
            let mut out = Vec::new();
            flate2::read::GzDecoder::new(raw.as_slice())
                .take(32_000_000)
                .read_to_end(&mut out)
                .map_err(|e| ClientError::Unexpected(format!("sticker store gzip: {e}")))?;
            out
        } else {
            raw
        };
        let all: Value = serde_json::from_slice(&bytes)?;
        Ok(StickerPack::list_from(&all, &excluded))
    }

    /// Stickers of one pack (`/api/message/sticker/category/detail`, `{cid}`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn get_sticker_category(&self, cate_id: i64) -> Result<Value, ClientError> {
        let plain = serde_json::json!({ "cid": cate_id }).to_string();
        self.get_sticker_raw("/api/message/sticker/category/detail", &plain)
            .await
    }

    /// One sticker with its image URLs (`/api/message/sticker/sticker_detail`, `{sid}`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn get_sticker_detail(&self, sticker_id: i64) -> Result<Value, ClientError> {
        let plain = serde_json::json!({ "sid": sticker_id }).to_string();
        self.get_sticker_raw("/api/message/sticker/sticker_detail", &plain)
            .await
    }

    /// Sticker list of one pack (`/api/message/sticker/category/sticker_detail`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn get_category_stickers(&self, cate_id: i64) -> Result<Value, ClientError> {
        let plain = format!("{{\"cid\":\"{cate_id}\"}}");
        self.get_sticker_raw("/api/message/sticker/category/sticker_detail", &plain)
            .await
    }

    /// Sticker search by keyword (`/api/message/sticker/search`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn search_stickers(&self, keyword: &str, limit: u32) -> Result<Value, ClientError> {
        let payload = serde_json::json!({
            "keyword": keyword,
            "limit": limit,
            "srcType": 0,
            "imei": self.device.imei,
        });
        let host = self.sticker_base().to_owned();
        self.get_encrypted(&host, "/api/message/sticker/search", &payload)
            .await
    }

    /// Stickers (and GIFs) suggested for what is being typed
    /// (`/api/message/sticker/suggest/stickers`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn suggest_stickers(&self, keyword: &str) -> Result<Value, ClientError> {
        let payload = serde_json::json!({
            "keyword": keyword,
            "gif": 1,
            "guggy": 0,
            "imei": self.device.imei,
        });
        let host = self.sticker_base().to_owned();
        self.get_encrypted(&host, "/api/message/sticker/suggest/stickers", &payload)
            .await
    }

    /// GIF categories (`/api/message/giphy/cates`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn get_gif_categories(&self, offset: u32, limit: u32) -> Result<Value, ClientError> {
        let plain = format!("{{\"offset\":{offset}, \"limit\":{limit}}}");
        self.get_sticker_raw("/api/message/giphy/cates", &plain)
            .await
    }

    /// GIFs of one category (`/api/message/giphy/gifs`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn get_gifs(
        &self,
        cate_id: i64,
        offset: u32,
        limit: u32,
    ) -> Result<Value, ClientError> {
        let plain = format!("{{\"cate_id\":{cate_id},\"offset\":{offset}, \"limit\":{limit}}}");
        self.get_sticker_raw("/api/message/giphy/gifs", &plain)
            .await
    }

    /// Trending GIFs (`/api/message/giphy/trending`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn get_gif_trending(&self, offset: u32, limit: u32) -> Result<Value, ClientError> {
        let plain = serde_json::json!({
            "offset": offset,
            "limit": limit,
            "imei": self.device.imei,
        })
        .to_string();
        self.get_sticker_raw("/api/message/giphy/trending", &plain)
            .await
    }

    /// GIF search (`/api/message/giphy/search`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn search_gifs(
        &self,
        keyword: &str,
        offset: u32,
        limit: u32,
    ) -> Result<Value, ClientError> {
        let plain = serde_json::json!({
            "keyword": keyword,
            "offset": offset,
            "limit": limit,
        })
        .to_string();
        self.get_sticker_raw("/api/message/giphy/search", &plain)
            .await
    }

    /// Sends a sticker (`/api/message/sticker`, or `/api/group/sticker`
    /// when `thread_id` is a group).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn send_sticker(
        &self,
        thread_id: &str,
        is_group: bool,
        sticker: &StickerRef,
        client_id: u64,
    ) -> Result<Value, ClientError> {
        let target = thread_id.strip_prefix("g_").unwrap_or(thread_id);
        let mut payload = serde_json::json!({
            "stickerId": sticker.id,
            "cateId": sticker.cate_id,
            "type": sticker.kind,
            "clientId": client_id,
            "imei": self.device.imei,
        });
        let (host, path) = if is_group {
            payload["grid"] = Value::from(target);
            (
                self.group_host
                    .as_deref()
                    .unwrap_or("https://tt-group-wpa.chat.zalo.me"),
                "/api/group/sticker",
            )
        } else {
            payload["toid"] = Value::from(target);
            (
                self.chat_host
                    .as_deref()
                    .unwrap_or("https://tt-chat2-wpa.chat.zalo.me"),
                "/api/message/sticker",
            )
        };
        let host = host.to_owned();
        self.post_encrypted(&host, path, &payload).await
    }

    /// Shares a GIF from the GIF store (`/api/message/gif` or
    /// `/api/group/gif`, encrypted params in the query like Zalo PC).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn send_gif(
        &self,
        thread_id: &str,
        is_group: bool,
        gif: &GifRef,
        client_id: u64,
    ) -> Result<Value, ClientError> {
        let target = thread_id.strip_prefix("g_").unwrap_or(thread_id);
        let mut payload = serde_json::json!({
            "thumb": gif.thumb,
            "width": gif.width,
            "height": gif.height,
            "msg": "",
            "type": 0,
            "href": gif.url,
            "contentId": gif.content_id,
            "src": 0,
            "clientId": client_id.to_string(),
        });
        if is_group {
            payload["grid"] = Value::from(target);
            payload["visibility"] = Value::from(0);
        } else {
            payload["toid"] = Value::from(target);
        }
        let host = self
            .file_host
            .as_deref()
            .unwrap_or("https://tt-files-wpa.chat.zalo.me")
            .to_owned();
        let path = if is_group {
            "/api/group/gif"
        } else {
            "/api/message/gif"
        };
        self.post_encrypted_query(&host, path, &payload).await
    }

    /// Downloads one https resource for the signed-in account.
    ///
    /// Used for friend avatars. The URL is not written to logs. Anything other
    /// than `https://`, or a body over 800 KB, is rejected.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when the URL is not https, the body is too
    /// large, or the transport fails.
    pub async fn fetch_asset(&self, url: &str) -> Result<Vec<u8>, ClientError> {
        self.fetch_limited(url, 800_000).await
    }

    /// Downloads one https resource of at most `max_bytes` (photos use a few MB).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when the URL is not https, the body is too
    /// large, or the transport fails.
    pub async fn fetch_limited(&self, url: &str, max_bytes: usize) -> Result<Vec<u8>, ClientError> {
        if !url.starts_with("https://") {
            return Err(ClientError::Unexpected(
                "asset URL must start with https://".to_owned(),
            ));
        }
        let response = self.http.get(url).send().await?;
        let status = response.status();
        if !status.is_success() {
            return Err(ClientError::Unexpected(format!("asset HTTP {status}")));
        }
        if response
            .content_length()
            .is_some_and(|len| len > max_bytes as u64)
        {
            return Err(ClientError::Unexpected("asset is too large".to_owned()));
        }
        let bytes = response.bytes().await?;
        if bytes.len() > max_bytes {
            return Err(ClientError::Unexpected("asset is too large".to_owned()));
        }
        Ok(bytes.to_vec())
    }

    /// Link preview the way Zalo PC builds link cards
    /// (`/api/message/parselink`, zca-js `parseLink`): `thumb`, `title`,
    /// `desc`, `src`, `href`.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn parse_link(&self, link: &str) -> Result<Value, ClientError> {
        let host = self
            .file_host
            .as_deref()
            .unwrap_or("https://tt-files-wpa.chat.zalo.me")
            .to_owned();
        let payload = serde_json::json!({
            "link": link,
            "version": 1,
            "continueOnError": true,
            "imei": self.device.imei,
        });
        self.get_encrypted(&host, "/api/message/parselink", &payload)
            .await
    }

    fn file_base(&self, is_group: bool) -> String {
        let host = self
            .file_host
            .as_deref()
            .unwrap_or("https://tt-files-wpa.chat.zalo.me");
        format!("{host}/api/{}/", if is_group { "group" } else { "message" })
    }

    /// Uploads one image (`photo_original/upload`, single chunk) for
    /// `thread_id` (a user id, or a group id with or without `g_`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors, or
    /// when the reply has no photo URL.
    pub async fn upload_photo(
        &self,
        thread_id: &str,
        is_group: bool,
        bytes: Vec<u8>,
        file_name: &str,
        client_id: u64,
    ) -> Result<PhotoUpload, ClientError> {
        let size = bytes.len();
        let target = thread_id.strip_prefix("g_").unwrap_or(thread_id);
        let mut payload = serde_json::json!({
            "totalChunk": 1,
            "fileName": file_name,
            "clientId": client_id,
            "totalSize": size,
            "imei": self.device.imei,
            "isE2EE": 0,
            "jxl": 0,
            "chunkId": 1,
        });
        payload[if is_group { "grid" } else { "toid" }] = Value::from(target);
        let mut query = self.common_params();
        query.insert(
            "type".to_owned(),
            if is_group { "11" } else { "2" }.to_owned(),
        );
        query.insert("params".to_owned(), self.encode_params(&payload)?);
        let url = format!("{}photo_original/upload", self.file_base(is_group));
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(file_name.to_owned())
            .mime_str("application/octet-stream")?;
        let form = reqwest::multipart::Form::new().part("chunkContent", part);
        let response = self
            .http
            .post(url)
            .query(&query)
            .multipart(form)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(ClientError::Unexpected(format!(
                "photo upload HTTP {status}: {body}"
            )));
        }
        let value: Value = serde_json::from_str(&body)?;
        ensure_success(&value)?;
        PhotoUpload::from_value(&self.decrypt_data(&value)?, size)
    }

    /// Sends an uploaded photo (`photo_original/send`) with an optional caption.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn send_photo(
        &self,
        thread_id: &str,
        is_group: bool,
        upload: &PhotoUpload,
        width: u32,
        height: u32,
        caption: &str,
    ) -> Result<Value, ClientError> {
        let target = thread_id.strip_prefix("g_").unwrap_or(thread_id);
        let mut payload = serde_json::json!({
            "photoId": upload.photo_id,
            "clientId": upload.client_file_id,
            "desc": caption,
            "width": width,
            "height": height,
            "rawUrl": upload.normal_url,
            "hdUrl": upload.hd_url,
            "thumbUrl": upload.thumb_url,
            "thumbSize": "53932",
            "fileSize": upload.size.to_string(),
            "hdSize": upload.size.to_string(),
            "zsource": -1,
            "ttl": 0,
            "jcp": "{\"convertible\":\"jxl\"}",
        });
        if is_group {
            payload["grid"] = Value::from(target);
            payload["oriUrl"] = Value::from(upload.normal_url.as_str());
        } else {
            payload["toid"] = Value::from(target);
            payload["normalUrl"] = Value::from(upload.normal_url.as_str());
        }
        let mut query = self.common_params();
        query.insert("nretry".to_owned(), "0".to_owned());
        let mut form = BTreeMap::new();
        form.insert("params".to_owned(), self.encode_params(&payload)?);
        let url = format!("{}photo_original/send", self.file_base(is_group));
        let (status, body) = self.post_form(url, &query, &form).await?;
        if !status.is_success() {
            return Err(ClientError::Unexpected(format!(
                "send photo HTTP {status}: {body}"
            )));
        }
        let value: Value = serde_json::from_str(&body)?;
        ensure_success(&value)?;
        self.decrypt_data(&value)
    }

    /// Uploads and sends one file (`sharefile_full`). Files larger than
    /// [`crate::SHAREFILE_MAX_BYTES`] are rejected; those use asyncfile on Zalo PC.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when the file is too large, or on transport,
    /// Zalo or decryption errors.
    pub async fn send_file(
        &self,
        thread_id: &str,
        is_group: bool,
        bytes: Vec<u8>,
        file_name: &str,
        client_id: u64,
    ) -> Result<FileUpload, ClientError> {
        let size = bytes.len() as u64;
        if size == 0 {
            return Err(ClientError::Unexpected("empty file".to_owned()));
        }
        if size > crate::SHAREFILE_MAX_BYTES {
            return Err(ClientError::Unexpected(format!(
                "file is larger than {} bytes",
                crate::SHAREFILE_MAX_BYTES
            )));
        }
        let payload = crate::ops::sharefile_payload(thread_id, is_group, client_id);
        let path = if is_group {
            "/api/group/sharefile_full"
        } else {
            "/api/message/sharefile_full"
        };
        self.post_file_content(path, is_group, payload, bytes, file_name, size)
            .await
    }

    /// Uploads a file with Zalo PC's async flow (`asyncfile/upload`,
    /// 3 MiB chunks) and returns the server `fileId`. The file URL arrives
    /// later as a `file_done` control on the socket
    /// ([`crate::listener::file_done_from_event`]); then call
    /// [`Self::send_async_file_msg`]. `sharefile_full` answers 404 for groups.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn upload_async_file(
        &self,
        thread_id: &str,
        is_group: bool,
        bytes: &[u8],
        file_name: &str,
        client_id: u64,
    ) -> Result<String, ClientError> {
        let url = format!("{}asyncfile/upload", self.file_base(is_group));
        let up = self
            .upload_chunks(
                &url, thread_id, is_group, bytes, file_name, client_id, "fileId",
            )
            .await?;
        if up.id.is_empty() {
            return Err(ClientError::Unexpected(
                "asyncfile/upload returned no fileId".to_owned(),
            ));
        }
        Ok(up.id)
    }

    /// Same async file upload as [`Self::upload_async_file`], reading 3 MiB
    /// chunks from disk so the whole file is not held in memory.
    ///
    /// Returns `(fileId, total size)`.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when the file is missing or empty, or on
    /// transport, Zalo or decryption errors.
    pub async fn upload_async_file_path(
        &self,
        thread_id: &str,
        is_group: bool,
        path: &std::path::Path,
        file_name: &str,
        client_id: u64,
    ) -> Result<(String, u64), ClientError> {
        let mut file = std::fs::File::open(path)
            .map_err(|e| ClientError::Unexpected(format!("open {}: {e}", path.display())))?;
        let total_size = file
            .metadata()
            .map_err(|e| ClientError::Unexpected(e.to_string()))?
            .len();
        if total_size == 0 {
            return Err(ClientError::Unexpected("empty file".to_owned()));
        }
        let chunk = crate::ops::ASYNC_CHUNK_BYTES;
        let total_chunks = crate::ops::async_chunk_count(total_size);
        let url = format!("{}asyncfile/upload", self.file_base(is_group));
        let mut file_id = String::new();
        let mut buf = vec![0u8; chunk as usize];
        for index in 0..total_chunks {
            let n = std::io::Read::read(&mut file, &mut buf)
                .map_err(|e| ClientError::Unexpected(e.to_string()))?;
            if n == 0 {
                break;
            }
            let id = self
                .post_upload_chunk(
                    &url,
                    thread_id,
                    is_group,
                    &buf[..n],
                    file_name,
                    client_id,
                    total_size,
                    total_chunks,
                    index,
                    "fileId",
                )
                .await?;
            if !id.is_empty() && id != "-1" {
                file_id = id;
            }
        }
        if file_id.is_empty() {
            return Err(ClientError::Unexpected(
                "asyncfile/upload returned no fileId".to_owned(),
            ));
        }
        Ok((file_id, total_size))
    }

    /// Uploads a video in 3 MiB chunks (`video/file/upload`, type 2 or 11).
    /// The playable URL arrives later as `video_done` (same shape as `file_done`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn upload_video_file(
        &self,
        thread_id: &str,
        is_group: bool,
        bytes: &[u8],
        file_name: &str,
        client_id: u64,
    ) -> Result<String, ClientError> {
        let url = format!("{}video/file/upload", self.file_base(is_group));
        let up = self
            .upload_chunks(
                &url, thread_id, is_group, bytes, file_name, client_id, "fileId",
            )
            .await?;
        if up.id.is_empty() {
            return Err(ClientError::Unexpected(
                "video/file/upload returned no fileId".to_owned(),
            ));
        }
        Ok(up.id)
    }

    /// Uploads a JPEG poster (`video/thumb/upload`). The URL is in the HTTP
    /// reply (`rawUrl` / `oriUrl` / `hdUrl` / `normalUrl`); there is no socket wait.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors, or
    /// when the reply has no image URL.
    pub async fn upload_video_thumb(
        &self,
        thread_id: &str,
        is_group: bool,
        bytes: &[u8],
        file_name: &str,
        client_id: u64,
    ) -> Result<String, ClientError> {
        let url = format!("{}video/thumb/upload", self.file_base(is_group));
        let up = self
            .upload_chunks(
                &url, thread_id, is_group, bytes, file_name, client_id, "photoId",
            )
            .await?;
        let thumb = upload_http_url(&up.root);
        if thumb.is_empty() {
            return Err(ClientError::Unexpected(
                "video/thumb/upload returned no url".to_owned(),
            ));
        }
        Ok(thumb)
    }

    /// Sends a video by forwarding it (`msgType` 5) on the file host after
    /// `video_done`. Direct uses `/api/message/forward`; a group uses
    /// `/api/group/forward`.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn forward_video(
        &self,
        thread_id: &str,
        is_group: bool,
        client_id: u64,
        info: &crate::ops::VideoSendInfo,
    ) -> Result<Value, ClientError> {
        let payload = crate::ops::forward_video_payload(
            thread_id,
            is_group,
            client_id,
            &self.device.imei,
            info,
        );
        let host = self
            .file_host
            .as_deref()
            .unwrap_or("https://tt-files-wpa.chat.zalo.me")
            .to_owned();
        let path = if is_group {
            "/api/group/forward"
        } else {
            "/api/message/forward"
        };
        self.post_encrypted(&host, path, &payload).await
    }

    /// One chunked upload (`chunkContent` + encrypted `params`), shared by
    /// async files and video. `id_key` is `fileId` or `photoId`. An id of
    /// `"-1"` is ignored so a later chunk can replace it.
    async fn upload_chunks(
        &self,
        url: &str,
        thread_id: &str,
        is_group: bool,
        bytes: &[u8],
        file_name: &str,
        client_id: u64,
        id_key: &str,
    ) -> Result<ChunkedUpload, ClientError> {
        const CHUNK: usize = crate::ops::ASYNC_CHUNK_BYTES as usize;
        if bytes.is_empty() {
            return Err(ClientError::Unexpected("empty file".to_owned()));
        }
        let total = bytes.len().div_ceil(CHUNK);
        let total_size = bytes.len() as u64;
        let mut file_id = String::new();
        let mut last = Value::Null;
        for (index, chunk) in bytes.chunks(CHUNK).enumerate() {
            let (id, data) = self
                .post_upload_chunk_data(
                    url, thread_id, is_group, chunk, file_name, client_id, total_size, total,
                    index, id_key,
                )
                .await?;
            if !id.is_empty() && id != "-1" {
                file_id = id;
            }
            last = data;
        }
        Ok(ChunkedUpload {
            id: file_id,
            root: last,
        })
    }

    /// One `chunkContent` POST. Returns the id field (possibly empty) and the
    /// decrypted body.
    async fn post_upload_chunk_data(
        &self,
        url: &str,
        thread_id: &str,
        is_group: bool,
        chunk: &[u8],
        file_name: &str,
        client_id: u64,
        total_size: u64,
        total_chunks: usize,
        index: usize,
        id_key: &str,
    ) -> Result<(String, Value), ClientError> {
        let target = thread_id.strip_prefix("g_").unwrap_or(thread_id);
        let mut payload = serde_json::json!({
            "totalChunk": total_chunks,
            "fileName": file_name,
            "clientId": client_id,
            "totalSize": total_size,
            "imei": self.device.imei,
            "isE2EE": 0,
            "jxl": 0,
            "chunkId": index + 1,
        });
        payload[if is_group { "grid" } else { "toid" }] = Value::from(target);
        let mut query = self.common_params();
        query.insert(
            "type".to_owned(),
            if is_group { "11" } else { "2" }.to_owned(),
        );
        query.insert("params".to_owned(), self.encode_params(&payload)?);
        let part = reqwest::multipart::Part::bytes(chunk.to_vec())
            .file_name(file_name.to_owned())
            .mime_str("application/octet-stream")?;
        let form = reqwest::multipart::Form::new().part("chunkContent", part);
        let response = self
            .http
            .post(url)
            .timeout(Duration::from_secs(90))
            .query(&query)
            .multipart(form)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(ClientError::Unexpected(format!(
                "{url} HTTP {status}: {body}"
            )));
        }
        let value: Value = serde_json::from_str(&body)?;
        ensure_success(&value)?;
        let data = self.decrypt_data(&value)?;
        let root = data.get("data").unwrap_or(&data);
        let id = json_id(root, id_key)
            .or_else(|| json_id(&data, id_key))
            .unwrap_or_default();
        Ok((id, data))
    }

    async fn post_upload_chunk(
        &self,
        url: &str,
        thread_id: &str,
        is_group: bool,
        chunk: &[u8],
        file_name: &str,
        client_id: u64,
        total_size: u64,
        total_chunks: u64,
        index: u64,
        id_key: &str,
    ) -> Result<String, ClientError> {
        let (id, _) = self
            .post_upload_chunk_data(
                url,
                thread_id,
                is_group,
                chunk,
                file_name,
                client_id,
                total_size,
                total_chunks as usize,
                index as usize,
                id_key,
            )
            .await?;
        Ok(id)
    }

    /// Sends the message for an uploaded async file (`asyncfile/msg`), with
    /// the fields of Zalo PC's `getFileAsyncItem` plus the `fileUrl` from
    /// `file_done`.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    #[allow(clippy::too_many_arguments)]
    pub async fn send_async_file_msg(
        &self,
        thread_id: &str,
        is_group: bool,
        file_id: &str,
        file_url: &str,
        file_name: &str,
        total_size: u64,
        checksum_md5: &str,
        client_id: u64,
    ) -> Result<Value, ClientError> {
        let target = thread_id.strip_prefix("g_").unwrap_or(thread_id);
        let extension = file_name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
        let mut payload = serde_json::json!({
            "fileId": file_id,
            "checksum": checksum_md5,
            "checksumSha": "",
            "extension": extension,
            "totalSize": total_size,
            "fileName": file_name,
            "clientId": client_id,
            "fType": 1,
            "fileCount": 0,
            "fdata": "{}",
            "fileUrl": file_url,
            "zsource": -1,
            "ttl": 0,
        });
        if is_group {
            payload["grid"] = Value::from(target);
        } else {
            payload["toid"] = Value::from(target);
            payload["imei"] = Value::from(self.device.imei.as_str());
        }
        let query = self.common_params();
        let mut form = BTreeMap::new();
        form.insert("params".to_owned(), self.encode_params(&payload)?);
        let url = format!("{}asyncfile/msg", self.file_base(is_group));
        let (status, body) = self.post_form(url, &query, &form).await?;
        if !status.is_success() {
            return Err(ClientError::Unexpected(format!(
                "asyncfile/msg HTTP {status}: {body}"
            )));
        }
        let value: Value = serde_json::from_str(&body)?;
        ensure_success(&value)?;
        self.decrypt_data(&value)
    }

    /// Uploads and sends a voice note (`voicemp3` / `group/sendmp3`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn send_voice(
        &self,
        thread_id: &str,
        is_group: bool,
        bytes: Vec<u8>,
        file_name: &str,
        client_id: u64,
    ) -> Result<FileUpload, ClientError> {
        let size = bytes.len() as u64;
        if size == 0 {
            return Err(ClientError::Unexpected("empty voice".to_owned()));
        }
        let payload = crate::ops::voice_payload(thread_id, is_group, client_id);
        let path = if is_group {
            "/api/group/sendmp3"
        } else {
            "/api/message/voicemp3"
        };
        self.post_file_content(path, is_group, payload, bytes, file_name, size)
            .await
    }

    /// Uploads the signed-in account's avatar (`/api/profile/upavatar`).
    /// Multipart field `fileContent`. Encrypted query `params` carries
    /// `avatarSize`, `clientId`, `language`, and `metaData` when `width`
    /// and `height` are both non-zero.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when the file is empty, under 7 KB or over
    /// 30 MB, or on transport, Zalo or decryption errors.
    pub async fn upload_profile_avatar(
        &self,
        bytes: Vec<u8>,
        file_name: &str,
        client_id: u64,
        width: u32,
        height: u32,
    ) -> Result<Value, ClientError> {
        if let Some(msg) = crate::ops::avatar_len_error(bytes.len()) {
            return Err(ClientError::Unexpected(msg.to_owned()));
        }
        let payload =
            crate::ops::profile_avatar_payload(client_id, width, height, bytes.len() as u64);
        self.post_avatar_file("/api/profile/upavatar", payload, bytes, file_name)
            .await
    }

    /// Uploads a group avatar (`/api/group/upavatar`). `thread_id` may
    /// include the `g_` prefix. Missing edges are sent as 240.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when the file is empty, under 7 KB or over
    /// 30 MB, or on transport, Zalo or decryption errors.
    pub async fn upload_group_avatar(
        &self,
        thread_id: &str,
        bytes: Vec<u8>,
        file_name: &str,
        client_id: u64,
        width: u32,
        height: u32,
    ) -> Result<Value, ClientError> {
        if let Some(msg) = crate::ops::avatar_len_error(bytes.len()) {
            return Err(ClientError::Unexpected(msg.to_owned()));
        }
        let payload = crate::ops::group_avatar_payload(
            thread_id,
            client_id,
            width,
            height,
            &self.device.imei,
        );
        self.post_avatar_file("/api/group/upavatar", payload, bytes, file_name)
            .await
    }

    /// One page of the conversation media store.
    /// Group and 1-1 use `/api/mediastore/list` (`media_store`).
    /// My Cloud uses `/api/media/oneone/list` (`media_store_send2me`).
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn media_store_list(
        &self,
        thread_id: &str,
        send2me: bool,
        media_type: &str,
        limit: Option<u32>,
        last_id: Option<&str>,
        last_fetch_id: Option<u64>,
    ) -> Result<crate::ops::MediaPage, ClientError> {
        let payload = crate::ops::media_list_payload(
            send2me,
            thread_id,
            media_type,
            limit,
            last_id,
            last_fetch_id,
            &self.device.imei,
        );
        let (host, path) = if send2me {
            (
                self.service_base(
                    "media_store_send2me",
                    crate::ops::MEDIA_STORE_SEND2ME_FALLBACK,
                )
                .to_owned(),
                "/api/media/oneone/list",
            )
        } else {
            (
                self.service_base("media_store", crate::ops::MEDIA_STORE_FALLBACK)
                    .to_owned(),
                "/api/mediastore/list",
            )
        };
        let value = self.get_encrypted(&host, path, &payload).await?;
        Ok(crate::ops::media_page_from(&value))
    }

    /// Photo, file and link preview for the info column.
    /// Group and 1-1: `POST /api/mediastore/preview` with `params` in the query.
    /// My Cloud: `POST /api/media/oneone/preview`.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn media_store_preview(
        &self,
        thread_id: &str,
        send2me: bool,
        limit: u32,
    ) -> Result<crate::ops::MediaPreview, ClientError> {
        let payload =
            crate::ops::media_preview_payload(send2me, thread_id, limit, &self.device.imei);
        let (host, path) = if send2me {
            (
                self.service_base(
                    "media_store_send2me",
                    crate::ops::MEDIA_STORE_SEND2ME_FALLBACK,
                )
                .to_owned(),
                "/api/media/oneone/preview",
            )
        } else {
            (
                self.service_base("media_store", crate::ops::MEDIA_STORE_FALLBACK)
                    .to_owned(),
                "/api/mediastore/preview",
            )
        };
        let value = self.post_encrypted_query(&host, path, &payload).await?;
        Ok(crate::ops::media_preview_from(&value))
    }

    /// Drops one stored item. `media_type` is `media`, `file` or `link`.
    /// Error 212 means the row is already gone.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] on transport, Zalo or decryption errors.
    pub async fn media_store_remove(
        &self,
        thread_id: &str,
        send2me: bool,
        media_type: &str,
        media_id: &str,
    ) -> Result<(), ClientError> {
        let payload = crate::ops::media_remove_payload(send2me, thread_id, media_type, media_id);
        let (host, path) = if send2me {
            (
                self.service_base(
                    "media_store_send2me",
                    crate::ops::MEDIA_STORE_SEND2ME_FALLBACK,
                )
                .to_owned(),
                "/api/media/oneone/remove",
            )
        } else {
            (
                self.service_base("media_store", crate::ops::MEDIA_STORE_FALLBACK)
                    .to_owned(),
                "/api/mediastore/remove",
            )
        };
        self.post_encrypted_query(&host, path, &payload).await?;
        Ok(())
    }

    /// POST `{file_host}{path}?params=<encrypted>` with multipart `fileContent`.
    /// Zalo PC also sets `Content-Type: application/x-www-form-urlencoded` on
    /// this call; that header would drop the multipart boundary, so the file
    /// part is sent the same way as `sharefile_full`.
    async fn post_avatar_file(
        &self,
        path: &str,
        payload: Value,
        bytes: Vec<u8>,
        file_name: &str,
    ) -> Result<Value, ClientError> {
        let mut query = self.common_params();
        query.insert("params".to_owned(), self.encode_params(&payload)?);
        let host = self
            .file_host
            .as_deref()
            .unwrap_or("https://tt-files-wpa.chat.zalo.me");
        let url = format!("{host}{path}");
        let name = if file_name.is_empty() {
            "avatar.jpg"
        } else {
            file_name
        };
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(name.to_owned())
            .mime_str("application/octet-stream")?;
        let form = reqwest::multipart::Form::new().part("fileContent", part);
        let response = self
            .http
            .post(url)
            .timeout(Duration::from_secs(60))
            .query(&query)
            .multipart(form)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(ClientError::Unexpected(format!(
                "{path} HTTP {status}: {body}"
            )));
        }
        let value: Value = serde_json::from_str(&body)?;
        ensure_success(&value)?;
        self.decrypt_data(&value)
    }

    async fn post_file_content(
        &self,
        path: &str,
        is_group: bool,
        payload: Value,
        bytes: Vec<u8>,
        file_name: &str,
        size: u64,
    ) -> Result<FileUpload, ClientError> {
        let mut query = self.common_params();
        query.insert(
            "type".to_owned(),
            if is_group { "11" } else { "2" }.to_owned(),
        );
        query.insert("params".to_owned(), self.encode_params(&payload)?);
        let host = self
            .file_host
            .as_deref()
            .unwrap_or("https://tt-files-wpa.chat.zalo.me");
        let url = format!("{host}{path}");
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(file_name.to_owned())
            .mime_str("application/octet-stream")?;
        let form = reqwest::multipart::Form::new().part("fileContent", part);
        let response = self
            .http
            .post(url)
            .timeout(Duration::from_secs(90))
            .query(&query)
            .multipart(form)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(ClientError::Unexpected(format!(
                "{path} HTTP {status}: {body}"
            )));
        }
        let value: Value = serde_json::from_str(&body)?;
        ensure_success(&value)?;
        FileUpload::from_value(&self.decrypt_data(&value)?, size)
    }

    /// Settings for [`crate::run_listener`]: the `zpw_ws` endpoints from
    /// getLoginInfo (or `server_info`), the cookie for the socket host, and the
    /// ping period from `server_info` when it has one.
    pub fn listener_config(&self, server_info: Option<&Value>) -> crate::ListenerConfig {
        use crate::listener::{ping_interval_from, ws_endpoints_from, DEFAULT_PING_INTERVAL};
        let mut endpoints = self.ws_endpoints.clone();
        if endpoints.is_empty() {
            endpoints = server_info.map(ws_endpoints_from).unwrap_or_default();
        }
        let host = endpoints
            .first()
            .and_then(|url| Url::parse(url).ok())
            .and_then(|url| url.host_str().map(str::to_owned))
            .unwrap_or_else(|| "ws1-msg.chat.zalo.me".to_owned());
        let cookie = Url::parse(&format!("https://{host}/"))
            .ok()
            .and_then(|url| self.jar.cookies(&url))
            .and_then(|value| value.to_str().ok().map(str::to_owned))
            .unwrap_or_default();
        crate::ListenerConfig {
            endpoints,
            cookie,
            user_agent: user_agent(),
            ping_interval: server_info
                .and_then(ping_interval_from)
                .unwrap_or(DEFAULT_PING_INTERVAL),
            since_user: None,
            since_group: None,
            cursor_path: None,
            max_unready_retries: None,
        }
    }

    fn common_params(&self) -> BTreeMap<String, String> {
        let mut params = BTreeMap::new();
        params.insert("zpw_ver".to_owned(), client_version());
        params.insert("zpw_type".to_owned(), api_type().to_string());
        params.insert("imei".to_owned(), self.device.imei.clone());
        params
    }

    fn encode_params(&self, payload: &Value) -> Result<String, ClientError> {
        let key = self
            .session_key
            .as_ref()
            .ok_or_else(|| ClientError::Unexpected("no session data key".to_owned()))?;
        let plaintext = serde_json::to_vec(payload)?;
        let ciphertext = aes_cbc_encrypt(key, &plaintext)?;
        Ok(base64_encode(&ciphertext))
    }

    fn decrypt_data(&self, value: &Value) -> Result<Value, ClientError> {
        let Some(encoded) = value.get("data").and_then(Value::as_str) else {
            return Ok(value.clone());
        };
        let key = self
            .session_key
            .as_ref()
            .ok_or_else(|| ClientError::Unexpected("no session data key".to_owned()))?;
        let ciphertext = base64_decode(encoded)?;
        let plaintext = aes_cbc_decrypt(key, &ciphertext)?;
        let decoded: Value = serde_json::from_slice(&plaintext)?;
        // The outer reply can be 0 while the decrypted one carries the real error.
        ensure_success(&decoded)?;
        Ok(decoded)
    }

    fn base_params(&self) -> BTreeMap<String, String> {
        let mut params = BTreeMap::new();
        params.insert("imei".to_owned(), self.device.imei.clone());
        params.insert("type".to_owned(), api_type().to_string());
        params.insert("client_version".to_owned(), client_version());
        params.insert(
            "computer_name".to_owned(),
            self.device.computer_name.clone(),
        );
        params
    }

    async fn get_text(
        &self,
        url: String,
        params: &BTreeMap<String, String>,
    ) -> Result<(StatusCode, String), ClientError> {
        let response = self.http.get(url).query(params).send().await?;
        let status = response.status();
        let body = response.text().await?;
        Ok((status, body))
    }

    async fn post_form(
        &self,
        url: String,
        query: &BTreeMap<String, String>,
        form: &BTreeMap<String, String>,
    ) -> Result<(StatusCode, String), ClientError> {
        let response = self.http.post(url).query(query).form(form).send().await?;
        let status = response.status();
        let body = response.text().await?;
        Ok((status, body))
    }
}

/// `data` of an encrypted getLoginInfo reply is ParamCipher ciphertext of
/// the whole inner reply (`{ error_code, data: {…} }`), as `decryptResp`
/// handles it. A reply whose `data` is already an object passes through.
fn decrypt_login_reply(cipher: &ParamCipher, value: Value) -> Result<Value, ClientError> {
    let Some(encoded) = value.get("data").and_then(Value::as_str) else {
        return Ok(value);
    };
    let inner = cipher
        .decrypt_json(encoded)
        .map_err(|e| ClientError::Unexpected(format!("getLoginInfo reply: {e}")))?;
    ensure_success(&inner)?;
    if inner.get("data").is_some_and(Value::is_object) {
        Ok(inner)
    } else {
        Ok(serde_json::json!({ "error_code": 0, "data": inner }))
    }
}

impl QrTicket {
    fn from_value(value: Value) -> Result<Self, ClientError> {
        let qr_url = string_field(&value, "qr_cnt").ok_or(ClientError::MissingField("qr_cnt"))?;
        let token_id = string_field(&value, "token_id").unwrap_or_default();
        let wait_scan = string_field(&value, "chk_wait_scan")
            .ok_or(ClientError::MissingField("chk_wait_scan"))?;
        let wait_confirm = string_field(&value, "chk_wait_cfirm")
            .or_else(|| string_field(&value, "chk_wait_cfm"))
            .or_else(|| string_field(&value, "chk_wait_confirm"))
            .ok_or_else(|| {
                ClientError::Unexpected(format!(
                    "QR response has no confirm URL; raw response: {value}"
                ))
            })?;
        let timeout_seconds = u64_field(&value, "qr_timeout").unwrap_or(180);
        let retry = u64_field(&value, "qr_retry").unwrap_or(3);
        Ok(Self {
            token_id,
            qr_url,
            wait_scan,
            wait_confirm,
            timeout_seconds,
            retry,
            raw: value,
        })
    }
}

fn field<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value
        .get(key)
        .or_else(|| value.get("data").and_then(|data| data.get(key)))
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    field(value, key).and_then(Value::as_str).map(str::to_owned)
}

fn u64_field(value: &Value, key: &str) -> Option<u64> {
    let value = field(value, key)?;
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}

fn zalo_error(value: &Value) -> Option<(i64, String)> {
    let code = field(value, "error_code")?.as_i64()?;
    let message = field(value, "error_message")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    Some((code, message))
}

fn ensure_success(value: &Value) -> Result<(), ClientError> {
    if let Some((code, message)) = zalo_error(value) {
        if code != 0 {
            return Err(ClientError::Zalo { code, message });
        }
    }
    Ok(())
}

fn with_language(url: &str) -> String {
    if url.contains('?') {
        format!("{url}&lang=vi")
    } else {
        format!("{url}?lang=vi")
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn first_host(map: Option<&Value>, service: &str) -> Option<String> {
    map.and_then(|map| map.get(service))
        .and_then(Value::as_array)
        .and_then(|hosts| hosts.first())
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// `JSON.stringify({})` from Electron when no local thread map exists.
pub(crate) fn empty_last_msgs_map() -> &'static str {
    "{}"
}

fn add_domain_cookie(jar: &Jar, name: &str, value: &str, domain: &str, anchor: &str) {
    let Ok(url) = Url::parse(anchor) else {
        return;
    };
    let cookie = format!("{name}={value}; Domain={domain}; Path=/");
    jar.add_cookie_str(&cookie, &url);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn empty_last_message_map_matches_electron_object() {
        assert_eq!(empty_last_msgs_map(), "{}");
    }

    #[test]
    fn important_payload_uses_last_id_until_a_newest_id_exists() {
        let first = important_payload("42", None, None);
        assert_eq!(first["limit"], 50);
        assert_eq!(first["uid"], "42");
        assert_eq!(first["lastId"], "-1");
        assert!(first.get("newestId").is_none());
        let next = important_payload("42", Some("99"), Some("-1"));
        assert_eq!(next["newestId"], "99");
        assert!(next.get("lastId").is_none());
    }

    #[test]
    fn reads_qr_fields_from_the_top_level_or_data() {
        let value = json!({
            "error_code": 0,
            "data": {
                "token_id": "zaloqr:v2_test",
                "qr_cnt": "http://zaloapp.com/qr/l?tk=test",
                "chk_wait_scan": "https://qr-id.zaloapp.com/polling/qr/waiting?code=test",
                "chk_wait_cfm": "https://qr-id.zaloapp.com/polling/qr/pc-status?code=test",
                "qr_timeout": "180",
                "qr_retry": 3
            }
        });
        let ticket = QrTicket::from_value(value).expect("ticket");
        assert_eq!(ticket.token_id, "zaloqr:v2_test");
        assert_eq!(ticket.timeout_seconds, 180);
        assert_eq!(ticket.retry, 3);
        assert!(ticket.wait_confirm.contains("pc-status"));
    }

    #[test]
    fn photo_upload_reply_reads_urls_and_falls_back() {
        let reply = json!({"normalUrl": "https://f/n.jpg", "photoId": 77, "clientFileId": "c1",
                           "thumbUrl": "https://f/t.jpg", "finished": 1});
        let up = PhotoUpload::from_value(&reply, 1234).expect("upload");
        assert_eq!(up.photo_id, "77");
        assert_eq!(up.hd_url, "https://f/n.jpg");
        assert_eq!(up.thumb_url, "https://f/t.jpg");
        assert_eq!(up.size, 1234);
        let thumb = json!({"data": {"oriUrl": "https://v/t.jpg", "photoId": 3}});
        assert_eq!(upload_http_url(&thumb), "https://v/t.jpg");
        assert_eq!(
            upload_http_url(&json!({"hdUrl": "http://x/a.jpg", "normalUrl": "https://x/b.jpg"})),
            "http://x/a.jpg"
        );
        let wrapped = json!({"data": {"normalUrl": "https://f/n.jpg", "photoId": "8"}});
        assert_eq!(
            PhotoUpload::from_value(&wrapped, 1)
                .expect("wrapped")
                .photo_id,
            "8"
        );
        assert!(PhotoUpload::from_value(&json!({"finished": 0}), 1).is_err());
    }

    #[test]
    fn file_upload_and_group_members_parse_bundle_shapes() {
        let reply =
            json!({"org": "https://f/a.bin", "fileId": 12, "fileSize": "44", "secureKey": "k"});
        let up = FileUpload::from_value(&reply, 1).expect("file");
        assert_eq!(up.file_id, "12");
        assert_eq!(up.file_url, "https://f/a.bin");
        assert_eq!(up.file_size, 44);
        assert_eq!(up.secure_key.as_deref(), Some("k"));
        let wrapped = json!({"data": {"hd": "https://f/b.bin", "fileId": "9"}});
        assert_eq!(
            FileUpload::from_value(&wrapped, 8)
                .expect("wrapped")
                .file_size,
            8
        );
        assert!(FileUpload::from_value(&json!({"finished": 0}), 1).is_err());

        let members = GroupMember::list_from(&json!({
            "data": {
                "gridInfoMap": {
                    "9": {
                        "currentMems": [
                            {"id": "111", "dName": "An", "avatar": "https://a/1.jpg"}
                        ]
                    }
                }
            }
        }));
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].uid, "111");
        assert_eq!(members[0].name, "An");
        let from_ver = GroupMember::list_from(&json!({
            "memVerList": ["222_0", "333_1"]
        }));
        assert_eq!(
            from_ver.iter().map(|m| m.uid.as_str()).collect::<Vec<_>>(),
            ["222", "333"]
        );

        let (ids, ver) = archived_ids_from(&json!({
            "data": {"version": 4, "ids": ["g9", "111"]}
        }));
        assert_eq!(ver, 4);
        assert_eq!(ids, ["g9", "111"]);
        let (items, _) = archived_ids_from(&json!({
            "items": [{"id": "9", "type": 1}, {"id": "111", "type": 0}]
        }));
        assert_eq!(items, ["g9", "111"]);

        let labels = ConversationLabel::list_from(&json!({
            "labelData": "[{\"id\":1,\"text\":\"A\",\"color\":\"#fff\",\"conversations\":[\"g1\"]}]"
        }));
        assert_eq!(labels.len(), 1);
        let data = ConversationLabel::to_label_data(&labels);
        assert!(data.contains("\"id\":1"));
        assert_eq!(
            ConversationLabel::version_of(&json!({"data": {"version": "7"}})),
            7
        );
    }

    #[test]
    fn sticker_and_gif_replies_parse() {
        let search = json!({"data": [{"cate_id": 10617, "sticker_id": 25826, "type": 7}]});
        let list = StickerRef::list_from(&search);
        assert_eq!(list.len(), 1);
        assert_eq!(
            (list[0].id, list[0].cate_id, list[0].kind),
            (25826, 10617, 7)
        );
        assert_eq!(
            list[0].image_url(),
            "https://zalo-api.zadn.vn/api/emoticon/sticker/webpc?eid=25826&size=130"
        );
        let suggest = json!({"data": {"sugg_sticker": [{"cate_id": 1, "sticker_id": 2, "type": 7}], "sugg_gif": null}});
        assert_eq!(StickerRef::list_from(&suggest).len(), 1);
        let pack =
            json!({"data": [{"cateId": 3, "id": 4, "type": 7, "stickerUrl": "https://x/4.png"}]});
        assert_eq!(
            StickerRef::list_from(&pack)[0].image_url(),
            "https://x/4.png"
        );
        let content = json!({"id": 5, "catId": 6, "type": 7});
        assert_eq!(StickerRef::from_value(&content).map(|s| s.cate_id), Some(6));
        let gifs = json!({"data": {"data_list": [{
            "id": "179",
            "normal": {"url": "https://g/a.gif", "width": 461, "height": 498},
            "preview": {"url": "https://g/a.png", "width": 461, "height": 498}
        }, {"id": "x", "normal": {"url": "http://insecure.gif"}}]}});
        let gifs = GifRef::list_from(&gifs);
        assert_eq!(gifs.len(), 1);
        assert_eq!(
            (gifs[0].thumb.as_str(), gifs[0].width),
            ("https://g/a.png", 461)
        );
        assert_eq!(gifs[0].content_id, json!("179"));
        let all = json!([
            {"id": 1, "name": " A ", "iconUrl": "https://i/1.png", "is_hidden": 0, "permission": 1},
            {"id": 2, "name": "hidden", "is_hidden": 5, "permission": 1},
            {"id": 3, "name": "excluded", "is_hidden": 0, "permission": 1}
        ]);
        let packs = StickerPack::list_from(&all, &[3]);
        assert_eq!(packs.len(), 1);
        assert_eq!(
            (packs[0].name.as_str(), packs[0].icon.as_str()),
            ("A", "https://i/1.png")
        );
    }

    #[test]
    fn builds_language_query() {
        assert_eq!(with_language("https://x/y"), "https://x/y?lang=vi");
        assert_eq!(with_language("https://x/y?a=1"), "https://x/y?a=1&lang=vi");
    }

    #[test]
    fn detects_zalo_errors() {
        let value = json!({"error_code": 18012, "error_message": "Invalid status"});
        assert!(matches!(
            ensure_success(&value),
            Err(ClientError::Zalo { code: 18012, .. })
        ));
        assert!(ensure_success(&json!({"error_code": 0})).is_ok());
    }

    #[test]
    fn get_login_info_is_encrypted_like_zalo_pc() {
        let device = Device {
            imei: "0123456789abcdef01234567".to_owned(),
            first_launch_time: 1_700_000_000,
            computer_name: "linux-test".to_owned(),
        };
        let session = StoredSession::from_cookie_headers(BTreeMap::new());
        let api = ZaloApi::from_session(device, &session).expect("build api");
        let cipher = ParamCipher::new(
            "0123456789abcdef01234567",
            1_700_000_000,
            api_type(),
            "a1b2c3d4e5",
        )
        .expect("cipher");
        let params = api.login_info_request(&cipher).expect("params");
        for key in [
            "zcid",
            "zcid_ext",
            "enc_ver",
            "params",
            "type",
            "client_version",
            "signkey",
        ] {
            assert!(params.contains_key(key), "missing {key}");
        }
        assert!(
            !params.contains_key("imei"),
            "imei only travels inside params"
        );
        let payload = cipher.decrypt_json(&params["params"]).expect("payload");
        assert_eq!(payload["imei"], "0123456789abcdef01234567");
        assert_eq!(payload["computer_name"], "linux-test");
        assert_eq!(payload["language"], "vi");
        assert!(payload["ts"]
            .as_u64()
            .is_some_and(|ts| ts > 1_700_000_000_000));

        // The reply's `data` is ciphertext of the inner reply.
        let inner = serde_json::json!({"error_code": 0, "data": {"uid": "1", "zpw_enk": "a2V5"}});
        let reply =
            serde_json::json!({"error_code": 0, "data": cipher.encrypt_json(&inner).expect("enc")});
        let opened = decrypt_login_reply(&cipher, reply).expect("decrypt");
        assert_eq!(opened["data"]["zpw_enk"], "a2V5");
        // An already-plain reply passes through.
        let plain = serde_json::json!({"error_code": 0, "data": {"uid": "1"}});
        assert_eq!(
            decrypt_login_reply(&cipher, plain.clone()).expect("plain"),
            plain
        );
    }

    #[test]
    fn login_info_reads_service_hosts_and_cipher_roundtrips() {
        let key_bytes = vec![7_u8; 32];
        let zpw_enk = base64_encode(&key_bytes);
        let value = json!({
            "error_code": 0,
            "data": {
                "dkey": "10009999",
                "zpw_enk": zpw_enk,
                "phone_number": "0900112233",
                "send2me_name": "Zalo Tester",
                "zpw_service_map_v3": {
                    "friend": ["https://friend.example.test"],
                    "profile": ["https://profile.example.test"],
                    "conversation": ["https://convers.example.test"],
                    "chat": ["https://chat.example.test"],
                    "group": ["https://group.example.test"],
                    "file": ["https://file.example.test"]
                },
                "zpw_ws": ["wss://ws3-msg.chat.zalo.me"]
            }
        });
        let info = LoginInfo::from_value(value).expect("parse login info");
        assert_eq!(info.uin, "10009999");
        assert_eq!(info.chat_host.as_deref(), Some("https://chat.example.test"));
        assert_eq!(
            info.group_host.as_deref(),
            Some("https://group.example.test")
        );
        assert_eq!(
            info.conversation_host.as_deref(),
            Some("https://convers.example.test")
        );
        assert_eq!(info.file_host.as_deref(), Some("https://file.example.test"));

        let mut session = StoredSession::from_cookie_headers(BTreeMap::new());
        let ws = info.ws_endpoints.clone();
        session.apply_login_info(info.uin, info.zpw_enk, info.phone_number, info.display_name);
        let api = ZaloApi::from_session(
            Device {
                imei: "imei-test".to_owned(),
                first_launch_time: 1_700_000_000,
                computer_name: "linux-test".to_owned(),
            },
            &session,
        )
        .expect("build api");
        let mut api = api;
        api.ws_endpoints = ws;
        api.set_zpw_sek("sek-test");
        let config = api.listener_config(None);
        assert_eq!(config.endpoints, vec!["wss://ws3-msg.chat.zalo.me"]);
        assert!(config.cookie.contains("zpw_sek=sek-test"));
        let payload = json!({"message": "Xin chào", "toid": "42", "clientId": 123});
        let encoded = api.encode_params(&payload).expect("encode params");
        let wrapped = json!({"error_code": 0, "data": encoded});
        let decrypted = api.decrypt_data(&wrapped).expect("decrypt data");
        assert_eq!(decrypted, payload);
        let failed = api
            .encode_params(&json!({"error_code": 114, "error_message": "Tham số không hợp lệ"}))
            .expect("encode error");
        assert!(matches!(
            api.decrypt_data(&json!({"error_code": 0, "data": failed})),
            Err(ClientError::Zalo { code: 114, .. })
        ));
    }
}
