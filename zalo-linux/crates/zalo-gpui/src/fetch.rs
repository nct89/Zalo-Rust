//! On-demand CDN fetch of photo / sticker / GIF thumbs for the open thread.
//!
//! Same contract as `zalo-gui` `photo_load_worker`: cache file
//! `~/.cache/zalo-rust/photos/<fnv(url)>`, `fetch_limited` 8 MB, at most
//! [`BUDGET`] requests in flight. Runs on GPUI's background executor with its
//! own current-thread tokio runtime. Never called at startup for preview
//! fixtures, and never from unit tests.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::RenderImage;
use zalo_client::{Device, StoredSession, ZaloApi};

use crate::media;

/// Parallel CDN requests per batch (`zalo-gui` budget).
pub const BUDGET: usize = 4;
/// Newest media items decoded / fetched per thread open. Older bubbles keep
/// the placeholder until message virtualization lands.
pub const MAX_PER_THREAD: usize = 48;
/// Response cap for full-resolution photos / GIFs / stickers.
const MAX_BYTES: usize = 25_000_000;

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
    // Only reuse an existing device file: a CDN thumb must never mint an IMEI.
    if !session_dir.join("device.json").is_file() {
        return Err("no saved device.json; skip CDN".into());
    }
    let device = Device::load_or_create(session_dir).map_err(|e| e.to_string())?;
    let api = match StoredSession::load(session_dir) {
        Ok(session) => ZaloApi::from_session(device, &session),
        Err(_) => ZaloApi::new(device),
    }
    .map_err(|e| e.to_string())?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    Ok(rt.block_on(async {
        let mut all = Vec::with_capacity(jobs.len());
        for chunk in jobs.chunks(BUDGET) {
            let futs = chunk
                .iter()
                .map(|(job, _)| api.fetch_limited(&job.url, MAX_BYTES));
            for res in futures::future::join_all(futs).await {
                all.push(res.ok().filter(|b| !b.is_empty()));
            }
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
