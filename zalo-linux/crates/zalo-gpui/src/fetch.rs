//! On-demand CDN fetch of photo / sticker / GIF thumbs for the open thread.
//!
//! Same contract as `zalo-gui` `photo_load_worker`: cache file
//! `~/.cache/zalo-rust/photos/<fnv(url)>`, `fetch_limited` 8 MB, at most
//! [`BUDGET`] requests in flight. Runs on GPUI's background executor with its
//! own current-thread tokio runtime. Never called at startup for preview
//! fixtures, and never from unit tests.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use gpui::RenderImage;
use zalo_client::{Device, StoredSession, ZaloApi};

use crate::media;

/// Parallel CDN requests per batch (`zalo-gui` budget).
pub const BUDGET: usize = 6;
/// Newest media items decoded / fetched per thread open. Older bubbles keep
/// the placeholder until message virtualization lands.
pub const MAX_PER_THREAD: usize = 48;
/// Response cap for full-resolution photos / GIFs / stickers.
const MAX_BYTES: usize = 25_000_000;
/// Per-asset download timeout so a stalled/expired URL never blocks a thread for 40s.
const FETCH_TIMEOUT: Duration = Duration::from_secs(15);

static FETCH_RT: OnceLock<Option<tokio::runtime::Runtime>> = OnceLock::new();
static SHARED_API: OnceLock<Mutex<Option<(PathBuf, Arc<ZaloApi>)>>> = OnceLock::new();

fn fetch_runtime() -> Option<&'static tokio::runtime::Runtime> {
    FETCH_RT
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .thread_name("zalo-media-fetch")
                .build()
                .ok()
        })
        .as_ref()
}

fn shared_api(session_dir: &Path) -> Result<Arc<ZaloApi>, String> {
    // Only reuse an existing device file: a CDN thumb must never mint an IMEI.
    if !session_dir.join("device.json").is_file() {
        return Err("no saved device.json; skip CDN".into());
    }
    let cache = SHARED_API.get_or_init(|| Mutex::new(None));
    if let Ok(guard) = cache.lock() {
        if let Some((dir, api)) = guard.as_ref() {
            if dir == session_dir {
                return Ok(Arc::clone(api));
            }
        }
    }
    let device = Device::load_or_create(session_dir).map_err(|e| e.to_string())?;
    let api = match StoredSession::load(session_dir) {
        Ok(session) => ZaloApi::from_session(device, &session),
        Err(_) => ZaloApi::new(device),
    }
    .map_err(|e| e.to_string())?;
    let api = Arc::new(api);
    if let Ok(mut guard) = cache.lock() {
        *guard = Some((session_dir.to_path_buf(), Arc::clone(&api)));
    }
    Ok(api)
}

static TMP_SEQ: AtomicU64 = AtomicU64::new(1);

/// Write `bytes` to a unique sibling temp file, verify it decodes, then
/// atomically rename into `dest` so concurrent readers never see half-written
/// bytes on disk.
fn save_and_decode_atomic(dest: &Path, bytes: &[u8], max_edge: u32) -> Option<Arc<RenderImage>> {
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let name = dest.file_name()?.to_string_lossy();
    let tmp = dest.with_file_name(format!(".{name}.part.{pid}.{seq}"));
    if std::fs::write(&tmp, bytes).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return None;
    }
    let img = media::decode_thumb(&tmp, max_edge);
    if img.is_some() {
        if std::fs::rename(&tmp, dest).is_err() {
            // Fallback on Windows if destination already exists from another thread.
            let _ = std::fs::copy(&tmp, dest);
            let _ = std::fs::remove_file(&tmp);
        }
    } else {
        let _ = std::fs::remove_file(&tmp);
    }
    img
}

/// Fetch and decode a single media job using the shared pooled HTTP client.
/// Returns immediately on cache hit; never blocks behind unrelated photos.
#[allow(dead_code)]
pub fn run_one(session_dir: Option<&Path>, job: Job) -> Done {
    run_one_with_retry(session_dir, job, false)
}

/// Fetch and decode a single media job with CDN propagation retry backoff.
///
/// Newly sent Zalo photos (`recent == true`) can arrive over WebSocket 1–2 s
/// before the CDN edge finishes replicating the object (`last-modified` trails
/// `sent_at`). Retrying transiently avoids falsely marking brand-new photos as
/// expired ("Ảnh đã hết hạn").
pub fn run_one_with_retry(session_dir: Option<&Path>, job: Job, recent: bool) -> Done {
    let path = job
        .path
        .clone()
        .filter(|p| p.is_file())
        .or_else(|| media::cache_path_for(&job.url));
    if let Some(local) = path.as_ref().filter(|p| p.is_file()) {
        if let Some(image) = media::decode_thumb(local, job.max_edge) {
            return Done {
                msg_id: job.msg_id,
                image: Some(image),
                path: Some(local.clone()),
            };
        }
        // Remove corrupt/zero-byte cache file so a fresh download can recover it.
        if job.path.is_none() {
            let _ = std::fs::remove_file(local);
        }
    }
    let mut done = Done {
        msg_id: job.msg_id,
        path: None,
        image: None,
    };
    let (Some(dir), Some(dest)) = (session_dir, path) else {
        return done;
    };
    if !media::is_http_url(&job.url) {
        return done;
    }
    let Ok(api) = shared_api(dir) else {
        return done;
    };
    let Some(rt) = fetch_runtime() else {
        return done;
    };
    let delays_ms: &[u64] = if recent {
        &[0, 500, 1_000, 1_800, 2_500]
    } else {
        &[0, 350]
    };
    for &delay_ms in delays_ms {
        let api = Arc::clone(&api);
        let url = job.url.clone();
        let bytes = rt.block_on(async move {
            if delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
            tokio::time::timeout(FETCH_TIMEOUT, api.fetch_limited(&url, MAX_BYTES))
                .await
                .ok()
                .and_then(|res| res.ok())
                .filter(|b| !b.is_empty())
        });
        if let Some(bytes) = bytes {
            if let Some(image) = save_and_decode_atomic(&dest, &bytes, job.max_edge) {
                done.image = Some(image);
                done.path = Some(dest);
                return done;
            }
        }
    }
    done
}

/// One bubble waiting for its image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    /// `ChatMsg::id`.
    pub msg_id: u64,
    /// Local file to decode (sent photo or cache hit). Wins over `url`.
    pub path: Option<PathBuf>,
    /// https thumb / HD URL. Empty when only `path` is known.
    pub url: String,
    /// Longest edge after decode.
    pub max_edge: u32,
}

/// Result for one job. `image` is `None` when the fetch or decode failed.
pub struct Done {
    /// `ChatMsg::id`.
    pub msg_id: u64,
    /// Cache file written (or already present).
    pub path: Option<PathBuf>,
    /// Decoded bitmap ready for `img()`.
    pub image: Option<Arc<RenderImage>>,
}

/// Decode local / cached files and fetch the rest. Blocking (JXL decode is
/// ~1 s per photo in debug builds): call from a background executor only.
/// `session_dir` `None` (preview) never touches the network.
pub fn run_batch(session_dir: Option<&Path>, jobs: Vec<Job>) -> Vec<Done> {
    let mut out = Vec::with_capacity(jobs.len());
    let mut missing = Vec::new();
    for job in jobs {
        let path = job
            .path
            .clone()
            .filter(|p| p.is_file())
            .or_else(|| media::cache_path_for(&job.url));
        match path.as_ref().filter(|p| p.is_file()) {
            Some(path) => out.push(Done {
                msg_id: job.msg_id,
                image: media::decode_thumb(path, job.max_edge),
                path: Some(path.clone()),
            }),
            None => missing.push((job, path)),
        }
    }
    if missing.is_empty() {
        return out;
    }
    let fetched = match session_dir
        .ok_or_else(|| "preview: no session dir".to_owned())
        .and_then(|dir| download(dir, &missing))
    {
        Ok(bytes) => bytes,
        Err(err) => {
            eprintln!("zalo-gpui fetch: {err}");
            vec![None; missing.len()]
        }
    };
    for ((job, path), bytes) in missing.into_iter().zip(fetched) {
        let mut done = Done {
            msg_id: job.msg_id,
            path: None,
            image: None,
        };
        if let (Some(bytes), Some(path)) = (bytes, path) {
            if std::fs::write(&path, &bytes).is_ok() {
                done.image = media::decode_thumb(&path, job.max_edge);
                if done.image.is_some() {
                    done.path = Some(path);
                } else {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
        out.push(done);
    }
    out
}

fn download(
    session_dir: &Path,
    jobs: &[(Job, Option<PathBuf>)],
) -> Result<Vec<Option<Vec<u8>>>, String> {
    let api = shared_api(session_dir)?;
    let rt = fetch_runtime().ok_or_else(|| "fetch runtime unavailable".to_owned())?;
    Ok(rt.block_on(async {
        let mut all = Vec::with_capacity(jobs.len());
        for chunk in jobs.chunks(BUDGET) {
            let futs = chunk.iter().map(|(job, _)| {
                let api = Arc::clone(&api);
                let url = job.url.clone();
                async move {
                    tokio::time::timeout(FETCH_TIMEOUT, api.fetch_limited(&url, MAX_BYTES))
                        .await
                        .ok()
                        .and_then(|res| res.ok())
                        .filter(|b| !b.is_empty())
                }
            });
            all.extend(futures::future::join_all(futs).await);
        }
        all
    }))
}

/// Keep the newest `cap` jobs (the visible message window).
#[must_use]
pub fn newest_n(mut jobs: Vec<Job>, cap: usize) -> Vec<Job> {
    if jobs.len() > cap {
        jobs = jobs.split_off(jobs.len() - cap);
    }
    jobs
}

/// Newest-first chunks of [`BUDGET`].
#[must_use]
#[allow(dead_code)]
pub fn plan(mut jobs: Vec<Job>) -> Vec<Vec<Job>> {
    // Newest first, so the bubbles nearest the bottom fill in before old ones.
    jobs.reverse();
    jobs.chunks(BUDGET).map(<[Job]>::to_vec).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(id: u64) -> Job {
        Job {
            msg_id: id,
            path: None,
            url: format!("https://t.example/{id}.jpg"),
            max_edge: 280,
        }
    }

    #[test]
    fn plan_caps_and_orders_newest_first() {
        let total = MAX_PER_THREAD as u64 + 6;
        let batches = plan(newest_n((0..total).map(job).collect(), MAX_PER_THREAD));
        let flat: Vec<u64> = batches.iter().flatten().map(|j| j.msg_id).collect();
        assert_eq!(flat.len(), MAX_PER_THREAD);
        assert_eq!(flat.first(), Some(&(total - 1)));
        assert_eq!(flat.last(), Some(&6));
        assert!(batches.iter().all(|b| b.len() <= BUDGET));
    }

    /// Opt-in network check: `ZALO_GPUI_FETCH_URL=https://… cargo test -p
    /// zalo-gpui -- --ignored live_fetch_one`. Uses the real saved session.
    #[test]
    #[ignore]
    fn live_fetch_one() {
        let url = std::env::var("ZALO_GPUI_FETCH_URL").expect("ZALO_GPUI_FETCH_URL");
        let dir = zalo_core::XdgDirs::from_env()
            .unwrap()
            .config_home()
            .join("zalo-rust");
        let done = run_batch(
            Some(&dir),
            vec![Job {
                msg_id: 1,
                path: None,
                url,
                max_edge: 280,
            }],
        );
        let path = done[0].path.clone().expect("cache file written");
        eprintln!(
            "cached {} ({} bytes)",
            path.display(),
            std::fs::metadata(&path).unwrap().len()
        );
        assert!(done[0].image.is_some(), "decoded");
    }

    #[test]
    fn missing_session_never_downloads() {
        let dir = std::env::temp_dir().join("zalo-gpui-fetch-test-empty");
        let _ = std::fs::create_dir_all(&dir);
        let err = download(&dir, &[(job(1), None)]).unwrap_err();
        assert!(err.contains("skip CDN"), "{err}");
        assert!(!dir.join("device.json").exists());
        // Preview (no session dir): a missing URL just fails, no download.
        let done = run_batch(None, vec![job(2)]);
        assert_eq!(done.len(), 1);
        assert!(done[0].image.is_none());
    }
}
