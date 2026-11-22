#!/usr/bin/env python3
"""Antigravity CLI (`agy`) autonomous PR worker for Zalo-Rust.

Polls open PRs belonging strictly to `nct89/Zalo-Rust` (never untrusted forks)
with label `upstream-sync` or `agy-auto`:
1. Implement phase: checks out the PR branch in an isolated `git worktree`,
   optionally unpacks the target upstream Zalo Desktop installer for `--add-dir`,
   and invokes `agy -p ... --mode auto --effort high` to implement upstream updates,
   update README.md, verify `cargo test --workspace`, commit, and push.
2. Review/CI fix phase: reads unresolved CodeRabbit (`coderabbitai[bot]`) review
   comments and failed GitHub Actions checks, invokes `agy` to resolve them,
   re-runs tests, commits, and pushes.
3. Completion gate: once CI is green and CodeRabbit has no unresolved comments,
   either marks `ready-to-merge` + sends a desktop notification (`AUTO_MERGE=false`,
   default) or squash-merges the PR (`AUTO_MERGE=true`) so `auto-release.yml`
   automatically tags and publishes a new GitHub Release.
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from typing import Any

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
DEFAULT_REPO_SLUG = "nct89/Zalo-Rust"
DEFAULT_AGY_BIN = os.environ.get("AGY_BIN") or shutil.which("agy") or "/home/nct89/.local/bin/agy"
DEFAULT_STATE_FILE = pathlib.Path(
    os.environ.get(
        "ZALO_BOT_STATE_FILE",
        os.path.expanduser("~/.local/state/zalo-rust-bot/state.json"),
    )
)
MAX_AGY_ROUNDS = int(os.environ.get("MAX_AGY_ROUNDS", "3"))
AUTO_MERGE = os.environ.get("AUTO_MERGE", "false").strip().lower() in ("1", "true", "yes")
AUTO_WATCH_UPSTREAM = os.environ.get("AUTO_WATCH_UPSTREAM", "true").strip().lower() in ("1", "true", "yes")
AUTO_TAG_RELEASE = os.environ.get("AUTO_TAG_RELEASE", "true").strip().lower() in ("1", "true", "yes")
AUTO_SYNC_LOCAL = os.environ.get("AUTO_SYNC_LOCAL", "true").strip().lower() in ("1", "true", "yes")
NOTIFY_RELEASES = os.environ.get("NOTIFY_RELEASES", "true").strip().lower() in ("1", "true", "yes")
DELETE_MERGED_BRANCH = os.environ.get("DELETE_MERGED_BRANCH", "true").strip().lower() in ("1", "true", "yes")
UPSTREAM_CHECK_INTERVAL_SEC = int(os.environ.get("UPSTREAM_CHECK_INTERVAL_SEC", "21600"))
RELEASE_CHECK_INTERVAL_SEC = int(os.environ.get("RELEASE_CHECK_INTERVAL_SEC", "900"))
AGY_TIMEOUT = os.environ.get("AGY_TIMEOUT", "30m")
AGY_EFFORT = os.environ.get("AGY_EFFORT", "high")
ELIGIBLE_LABELS = {"upstream-sync", "agy-auto"}
BLOCKING_LABELS = {"needs-human", "ready-to-merge"}
RELEASE_LABELS = {"release:patch", "release:minor", "release:major"}
AUTOMATION_LABELS = [
    ("upstream-sync", "0068ff", "Automated upstream Zalo Desktop update analysis"),
    ("agy-auto", "7c3aed", "Eligible for Antigravity CLI autonomous PR worker"),
    ("release:patch", "0e8a16", "Auto-bump patch version and publish release on merge"),
    ("release:minor", "1d76db", "Auto-bump minor version and publish release on merge"),
    ("ready-to-merge", "2ea043", "CI green and AI review clean"),
    ("needs-human", "d93f0b", "Requires manual developer intervention"),
]


class GitHubClient:
    def __init__(self, repo_slug: str, token: str) -> None:
        self.repo_slug = repo_slug
        self.token = token

    def _req(self, method: str, path: str, body: dict[str, Any] | None = None) -> Any:
        url = path if path.startswith("https://") else f"https://api.github.com/repos/{self.repo_slug}{path}"
        headers = {
            "Accept": "application/vnd.github+json",
            "User-Agent": "zalo-rust-agy-pr-bot/1.0",
            "X-GitHub-Api-Version": "2022-11-28",
        }
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"
        data = None
        if body is not None:
            data = json.dumps(body).encode("utf-8")
            headers["Content-Type"] = "application/json"
        req = urllib.request.Request(url, data=data, headers=headers, method=method)
        with urllib.request.urlopen(req, timeout=30) as resp:
            raw = resp.read().decode("utf-8", errors="replace")
            return json.loads(raw) if raw.strip() else {}

    def list_open_prs(self) -> list[dict[str, Any]]:
        return self._req("GET", "/pulls?state=open&base=main&per_page=30")

    def get_pr(self, pr_num: int) -> dict[str, Any]:
        return self._req("GET", f"/pulls/{pr_num}")

    def list_review_comments(self, pr_num: int) -> list[dict[str, Any]]:
        return self._req("GET", f"/pulls/{pr_num}/comments?per_page=100")

    def list_reviews(self, pr_num: int) -> list[dict[str, Any]]:
        return self._req("GET", f"/pulls/{pr_num}/reviews?per_page=50")

    def list_check_runs(self, head_sha: str) -> list[dict[str, Any]]:
        data = self._req("GET", f"/commits/{head_sha}/check-runs?per_page=100")
        return data.get("check_runs", [])

    def get_job_log_tail(self, job_id: int, max_chars: int = 4000) -> str:
        url = f"https://api.github.com/repos/{self.repo_slug}/actions/jobs/{job_id}/logs"
        headers = {
            "Accept": "application/vnd.github+json",
            "User-Agent": "zalo-rust-agy-pr-bot/1.0",
            "X-GitHub-Api-Version": "2022-11-28",
        }
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"
        req = urllib.request.Request(url, headers=headers, method="GET")
        try:
            with urllib.request.urlopen(req, timeout=30) as resp:
                raw = resp.read().decode("utf-8", errors="replace")
            return raw[-max_chars:] if len(raw) > max_chars else raw
        except Exception:
            return ""

    def post_issue_comment(self, pr_num: int, body: str) -> None:
        self._req("POST", f"/issues/{pr_num}/comments", {"body": body})

    def add_labels(self, pr_num: int, labels: list[str]) -> None:
        self._req("POST", f"/issues/{pr_num}/labels", {"labels": labels})

    def squash_merge(self, pr_num: int, head_sha: str, title: str) -> Any:
        return self._req(
            "PUT",
            f"/pulls/{pr_num}/merge",
            {
                "commit_title": title,
                "sha": head_sha,
                "merge_method": "squash",
            },
        )

    def delete_ref(self, ref: str) -> None:
        try:
            self._req("DELETE", f"/git/refs/{ref}")
        except urllib.error.HTTPError as err:
            if err.code not in (404, 422):
                raise

    def ensure_label(self, name: str, color: str, description: str) -> None:
        try:
            self._req("POST", "/labels", {"name": name, "color": color, "description": description})
        except urllib.error.HTTPError as err:
            if err.code != 422:
                raise

    def list_open_prs_by_head(self, branch: str) -> list[dict[str, Any]]:
        owner = self.repo_slug.split("/", 1)[0]
        return self._req("GET", f"/pulls?state=open&head={owner}:{branch}&per_page=10")

    def create_pr(self, title: str, head: str, base: str, body: str) -> dict[str, Any]:
        return self._req("POST", "/pulls", {"title": title, "head": head, "base": base, "body": body})

    def list_tags(self, per_page: int = 10) -> list[dict[str, Any]]:
        return self._req("GET", f"/tags?per_page={per_page}")

    def get_latest_release(self) -> dict[str, Any] | None:
        try:
            return self._req("GET", "/releases/latest")
        except urllib.error.HTTPError as err:
            if err.code == 404:
                return None
            raise

    def get_release_by_tag(self, tag: str) -> dict[str, Any] | None:
        try:
            return self._req("GET", f"/releases/tags/{tag}")
        except urllib.error.HTTPError as err:
            if err.code == 404:
                return None
            raise

    def list_workflow_runs(self, per_page: int = 20) -> list[dict[str, Any]]:
        data = self._req("GET", f"/actions/runs?per_page={per_page}")
        return data.get("workflow_runs", [])


def notify_desktop(summary: str, body: str) -> None:
    if shutil.which("notify-send"):
        subprocess.run(
            ["notify-send", "-a", "Zalo-Rust Bot", summary, body],
            check=False,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )


def load_state(path: pathlib.Path) -> dict[str, Any]:
    if path.is_file():
        try:
            return json.loads(path.read_text(encoding="utf-8"))
        except Exception:
            pass
    return {"prs": {}}


def save_state(path: pathlib.Path, state: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(".tmp")
    tmp.write_text(json.dumps(state, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    tmp.replace(path)


def is_eligible_pr(pr: dict[str, Any], repo_slug: str) -> tuple[bool, str]:
    if pr.get("draft"):
        return False, "draft PR"
    head_repo = ((pr.get("head") or {}).get("repo") or {}).get("full_name", "")
    if head_repo.lower() != repo_slug.lower():
        return False, f"untrusted fork ({head_repo})"
    labels = {lb.get("name", "") for lb in pr.get("labels", [])}
    if labels & BLOCKING_LABELS:
        return False, f"blocked by label {sorted(labels & BLOCKING_LABELS)}"
    if not (labels & ELIGIBLE_LABELS):
        return False, "missing upstream-sync / agy-auto label"
    return True, "ok"


def inspect_pr_signals(
    gh: GitHubClient,
    pr: dict[str, Any],
    addressed_comment_ids: set[int] | None = None,
) -> dict[str, Any]:
    addressed = addressed_comment_ids or set()
    pr_num = int(pr["number"])
    head_sha = pr["head"]["sha"]
    checks = gh.list_check_runs(head_sha)
    ci_checks_count = 0
    failed_checks = []
    pending_checks = []
    coderabbit_check_completed = False
    for ck in checks:
        name = ck.get("name", "")
        status = ck.get("status", "")
        conclusion = ck.get("conclusion")
        if "coderabbit" in name.lower():
            if status == "completed" and conclusion in ("success", "neutral", "skipped"):
                coderabbit_check_completed = True
            continue
        ci_checks_count += 1
        if status != "completed":
            pending_checks.append(name)
        elif conclusion not in ("success", "neutral", "skipped"):
            entry: dict[str, Any] = {
                "name": name,
                "conclusion": conclusion,
                "details_url": ck.get("html_url", ""),
            }
            if ck.get("id"):
                log_tail = gh.get_job_log_tail(int(ck["id"]))
                if log_tail:
                    entry["log_tail"] = log_tail
            failed_checks.append(entry)

    reviews = gh.list_reviews(pr_num)
    coderabbit_reviews = [
        r for r in reviews if "coderabbit" in ((r.get("user") or {}).get("login", "").lower())
    ]
    coderabbit_reviewed_head = coderabbit_check_completed or any(
        r.get("commit_id") == head_sha for r in coderabbit_reviews
    )

    comments = gh.list_review_comments(pr_num)
    coderabbit_comments = [
        {
            "id": c.get("id"),
            "path": c.get("path", ""),
            "line": c.get("line") or c.get("original_line"),
            "commit_id": c.get("commit_id", ""),
            "original_commit_id": c.get("original_commit_id", ""),
            "body": c.get("body", "")[:1800],
        }
        for c in comments
        if "coderabbit" in ((c.get("user") or {}).get("login", "").lower())
        and c.get("commit_id") == head_sha
        and c.get("id") not in addressed
    ]

    return {
        "head_sha": head_sha,
        "ci_checks_count": ci_checks_count,
        "pending_checks": pending_checks,
        "failed_checks": failed_checks,
        "coderabbit_reviewed_head": coderabbit_reviewed_head,
        "coderabbit_comments": coderabbit_comments,
    }


def detect_upstream_report(worktree: pathlib.Path) -> pathlib.Path | None:
    report_dir = worktree / "docs" / "upstream-reports"
    if not report_dir.is_dir():
        return None
    reports = sorted(report_dir.glob("zalo-*.md"), key=lambda p: p.stat().st_mtime, reverse=True)
    return reports[0] if reports else None


def build_agy_prompt(
    pr: dict[str, Any],
    phase: str,
    signals: dict[str, Any],
    report_rel: str | None,
) -> str:
    pr_num = pr["number"]
    pr_title = pr["title"]
    if phase == "implement":
        extra_feedback = ""
        if signals["coderabbit_comments"] or signals["failed_checks"]:
            feedback_json = json.dumps(
                {
                    "failed_checks": signals["failed_checks"],
                    "coderabbit_comments": signals["coderabbit_comments"],
                },
                indent=2,
                ensure_ascii=False,
            )
            extra_feedback = (
                "\n- Đồng thời xử lý toàn bộ nhận xét review hiện có từ CodeRabbit và lỗi CI bên dưới:\n"
                f"```json\n{feedback_json}\n```\n"
            )
        return (
            f"Bạn đang xử lý tự động Pull Request #{pr_num} ({pr_title}) của repo Zalo-Rust.\n"
            f"- Đọc kỹ báo cáo phân tích upstream tại `{report_rel}` (và thư mục unpacked đi kèm nếu có qua --add-dir).\n"
            "- Thực hiện các cập nhật cần thiết cho `core/` (protocol, client, sync, backup, design) và `zalo-linux/crates/zalo-gpui` "
            "để tương thích bản Zalo Desktop mới (giữ nguyên `#![forbid(unsafe_code)]` ở `core/`, dùng 100% `token_rgb` cho UI, giữ biên dịch chéo Linux/macOS/Windows).\n"
            "- Không đưa `zalo-multi` hay ghi âm cuộc gọi vào `Zalo-Rust`.\n"
            "- Cập nhật `README.md` nếu có thay đổi tính năng/cấu hình.\n"
            f"{extra_feedback}"
            "- Chạy `cargo test --workspace -- --test-threads=1` và đảm bảo pass 100% trước khi kết thúc.\n"
            "- KHÔNG tự chạy `git push`; chỉ sửa mã nguồn sạch sẽ trong thư mục hiện tại."
        )
    issues_json = json.dumps(
        {
            "failed_checks": signals["failed_checks"],
            "coderabbit_comments": signals["coderabbit_comments"],
        },
        indent=2,
        ensure_ascii=False,
    )
    return (
        f"Bạn đang xử lý các nhận xét review từ CodeRabbit và/hoặc lỗi CI trên Pull Request #{pr_num} ({pr_title}).\n"
        f"Dữ liệu chi tiết:\n```json\n{issues_json}\n```\n"
        "- Đọc từng comment của CodeRabbit và sửa triệt để các lỗi thật sự (bỏ qua gợi ý sai ngữ cảnh nhưng phải giải thích ngắn gọn).\n"
        "- Sửa mọi lỗi biên dịch/test từ CI (chú ý tương thích Linux/macOS/Windows).\n"
        "- Chạy `cargo test --workspace -- --test-threads=1` và xác minh pass 100%.\n"
        "- KHÔNG tự chạy `git push`; chỉ chỉnh sửa file trong thư mục hiện tại."
    )


def run_agy_on_worktree(
    agy_bin: str,
    worktree: pathlib.Path,
    prompt: str,
    extra_dir: pathlib.Path | None,
    dry_run: bool,
) -> tuple[bool, str]:
    cmd = [
        agy_bin,
        "-p",
        prompt,
        "--mode",
        "auto",
        "--effort",
        AGY_EFFORT,
        "--output-format",
        "json",
        "--print-timeout",
        AGY_TIMEOUT,
    ]
    if extra_dir and extra_dir.is_dir():
        cmd += ["--add-dir", str(extra_dir)]
    if dry_run:
        print(f"[DRY-RUN] cwd={worktree} cmd={' '.join(cmd[:6])} ... (prompt_len={len(prompt)})")
        return True, "dry-run"
    proc = subprocess.run(cmd, cwd=worktree, capture_output=True, text=True, check=False)
    output = (proc.stdout or "") + ("\n" + proc.stderr if proc.stderr else "")
    return proc.returncode == 0, output[-4000:]


def git_remote_args(repo_slug: str, token: str) -> tuple[list[str], str]:
    """Build non-interactive HTTPS auth flags so git fetch/push works under systemd without SSH agent."""
    if token:
        basic = base64.b64encode(f"x-access-token:{token}".encode("utf-8")).decode("ascii")
        return (
            ["-c", f"http.https://github.com/.extraheader=AUTHORIZATION: basic {basic}"],
            f"https://github.com/{repo_slug}.git",
        )
    return ([], "origin")


def verify_and_push_worktree(
    worktree: pathlib.Path,
    repo_slug: str,
    token: str,
    branch: str,
    commit_msg: str,
    dry_run: bool,
) -> tuple[bool, str, str]:
    if dry_run:
        return True, "dry-run", ""
    status = subprocess.run(
        ["git", "status", "--porcelain"], cwd=worktree, capture_output=True, text=True, check=True
    ).stdout.strip()
    if not status:
        return True, "no-changes", ""

    env = os.environ.copy()
    cargo_bin = shutil.which("cargo") or os.path.expanduser("~/.cargo/bin/cargo")
    test_proc = subprocess.run(
        [cargo_bin, "test", "--workspace", "--", "--test-threads=1"],
        cwd=worktree,
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )
    if test_proc.returncode != 0:
        return False, f"cargo test failed:\n{(test_proc.stdout + test_proc.stderr)[-3000:]}", ""

    subprocess.run(["git", "add", "-A"], cwd=worktree, check=True)
    subprocess.run(["git", "commit", "-m", commit_msg], cwd=worktree, check=True)
    new_sha = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=worktree, capture_output=True, text=True, check=True
    ).stdout.strip()
    extra, remote = git_remote_args(repo_slug, token)
    subprocess.run(["git", *extra, "push", remote, f"HEAD:{branch}"], cwd=worktree, check=True)
    return True, "pushed", new_sha


def process_pr(
    gh: GitHubClient,
    repo: pathlib.Path,
    agy_bin: str,
    pr: dict[str, Any],
    state: dict[str, Any],
    dry_run: bool,
) -> None:
    pr_num = int(pr["number"])
    branch = pr["head"]["ref"]
    head_sha = pr["head"]["sha"]
    pr_key = str(pr_num)
    pr_state = state["prs"].setdefault(
        pr_key,
        {
            "rounds": 0,
            "implemented": False,
            "last_processed_sha": "",
            "last_pushed_sha": "",
            "last_push_ts": 0,
            "addressed_comment_ids": [],
        },
    )
    pr_state.setdefault("addressed_comment_ids", [])
    pr_state.setdefault("last_pushed_sha", "")
    pr_state.setdefault("last_push_ts", 0)

    if (
        pr_state["last_pushed_sha"] == head_sha
        and time.time() - float(pr_state["last_push_ts"]) < 90
    ):
        print(f"PR #{pr_num}: cooling down after push of {head_sha[:7]}, waiting for CI/CodeRabbit.")
        return

    addressed_ids = {int(cid) for cid in pr_state["addressed_comment_ids"]}
    signals = inspect_pr_signals(gh, pr, addressed_comment_ids=addressed_ids)
    if signals["ci_checks_count"] == 0:
        print(f"PR #{pr_num}: waiting for CI check-runs to start on {head_sha[:7]}")
        return
    if signals["pending_checks"]:
        print(f"PR #{pr_num}: waiting for pending CI checks: {signals['pending_checks']}")
        return

    labels = {lb.get("name", "") for lb in pr.get("labels", [])}
    needs_implement = "upstream-sync" in labels and not pr_state.get("implemented", False)
    needs_fix = bool(signals["failed_checks"]) or bool(signals["coderabbit_comments"])

    # If already implemented and CI is green and CodeRabbit reviewed head with 0 comments -> ready!
    if not needs_implement and not needs_fix:
        if not signals["coderabbit_reviewed_head"]:
            print(f"PR #{pr_num}: CI green, waiting for CodeRabbit review on {head_sha[:7]}")
            return
        print(f"PR #{pr_num}: CI green & CodeRabbit clean!")
        if dry_run:
            print(f"[DRY-RUN] Would mark ready-to-merge (AUTO_MERGE={AUTO_MERGE})")
            return
        if AUTO_MERGE:
            gh.squash_merge(pr_num, head_sha, f"{pr['title']} (#{pr_num})")
            print(f"PR #{pr_num}: squash-merged into main.")
            if DELETE_MERGED_BRANCH and branch and branch != "main":
                gh.delete_ref(f"heads/{branch}")
                print(f"PR #{pr_num}: deleted remote branch {branch}.")
            notify_desktop(f"Zalo-Rust PR #{pr_num} đã tự Merge", f"{pr['title']} — đang tạo bản phát hành.")
            if AUTO_TAG_RELEASE and (labels & RELEASE_LABELS):
                bump_and_push_release_tag(gh, repo, labels, state, dry_run=dry_run)
        else:
            gh.add_labels(pr_num, ["ready-to-merge"])
            gh.post_issue_comment(
                pr_num,
                "CI đã xanh và CodeRabbit không còn góp ý nào cần xử lý. PR đã sẵn sàng để bạn duyệt và Merge!",
            )
            notify_desktop(
                f"Zalo-Rust PR #{pr_num} sẵn sàng Merge",
                f"{pr['title']}\nCI xanh + CodeRabbit sạch.",
            )
        return

    if pr_state["last_processed_sha"] == head_sha and not needs_implement:
        print(f"PR #{pr_num}: already processed head {head_sha[:7]}, skipping.")
        return

    if pr_state["rounds"] >= MAX_AGY_ROUNDS:
        print(f"PR #{pr_num}: reached MAX_AGY_ROUNDS={MAX_AGY_ROUNDS}, flagging needs-human.")
        if not dry_run:
            gh.add_labels(pr_num, ["needs-human"])
            gh.post_issue_comment(
                pr_num,
                f"Antigravity CLI bot đã chạy tối đa {MAX_AGY_ROUNDS} vòng nhưng vẫn còn lỗi CI/review. Đã gắn nhãn `needs-human` để bạn kiểm tra thủ công.",
            )
            notify_desktop(f"Zalo-Rust PR #{pr_num} cần kiểm tra tay", pr["title"])
        return

    phase = "implement" if needs_implement else "fix-review"
    print(f"PR #{pr_num} ({branch}): running phase={phase} (round {pr_state['rounds'] + 1}/{MAX_AGY_ROUNDS})")

    if dry_run:
        prompt = build_agy_prompt(pr, phase, signals, "docs/upstream-reports/zalo-latest.md")
        run_agy_on_worktree(agy_bin, repo, prompt, None, dry_run=True)
        return

    extra, remote = git_remote_args(gh.repo_slug, gh.token)
    subprocess.run(["git", *extra, "fetch", remote, branch], cwd=repo, check=True)
    wt_dir = pathlib.Path(tempfile.mkdtemp(prefix=f"zalo-rust-pr-{pr_num}-"))
    unpacked_dir: pathlib.Path | None = None
    try:
        subprocess.run(
            ["git", "worktree", "add", "--detach", str(wt_dir), "FETCH_HEAD"],
            cwd=repo,
            check=True,
        )
        report = detect_upstream_report(wt_dir)
        report_rel = str(report.relative_to(wt_dir)) if report else None

        if phase == "implement" and report is not None:
            unpacked_dir = pathlib.Path(tempfile.mkdtemp(prefix=f"zalo-upstream-pr-{pr_num}-"))
            subprocess.run(
                [
                    sys.executable,
                    str(wt_dir / "scripts" / "zalo-upstream-diff.py"),
                    "--repo",
                    str(wt_dir),
                    "--download",
                    "--force",
                    "--keep-unpacked",
                    str(unpacked_dir),
                ],
                check=False,
            )

        prompt = build_agy_prompt(pr, phase, signals, report_rel)
        ok, agy_out = run_agy_on_worktree(agy_bin, wt_dir, prompt, unpacked_dir, dry_run=False)
        if not ok:
            gh.post_issue_comment(
                pr_num,
                f"Antigravity CLI (`agy`) gặp lỗi ở pha `{phase}`:\n```\n{agy_out[-1500:]}\n```",
            )
            return

        msg = (
            f"feat(upstream): apply automated updates for PR #{pr_num} via agy"
            if phase == "implement"
            else f"fix(review): address CodeRabbit & CI feedback on PR #{pr_num} via agy"
        )
        pushed_ok, push_note, new_sha = verify_and_push_worktree(
            wt_dir, gh.repo_slug, gh.token, branch, msg, dry_run=False
        )
        pr_state["rounds"] += 1
        pr_state["last_processed_sha"] = head_sha
        if phase == "implement":
            pr_state["implemented"] = True
        for c in signals["coderabbit_comments"]:
            cid = c.get("id")
            if cid is not None and cid not in pr_state["addressed_comment_ids"]:
                pr_state["addressed_comment_ids"].append(cid)
        if new_sha:
            pr_state["last_pushed_sha"] = new_sha
            pr_state["last_push_ts"] = int(time.time())

        if not pushed_ok:
            gh.post_issue_comment(
                pr_num,
                f"Antigravity CLI đã sửa mã nguồn nhưng `cargo test --workspace` thất bại:\n```\n{push_note[-2000:]}\n```",
            )
        elif push_note == "pushed":
            gh.post_issue_comment(
                pr_num,
                f"Antigravity CLI (`agy`) đã hoàn tất pha `{phase}` (vòng {pr_state['rounds']}/{MAX_AGY_ROUNDS}), vượt qua `cargo test --workspace` và push commit mới. @coderabbitai review",
            )
    finally:
        subprocess.run(
            ["git", "worktree", "remove", "--force", str(wt_dir)],
            cwd=repo,
            check=False,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        shutil.rmtree(wt_dir, ignore_errors=True)
        if unpacked_dir is not None:
            shutil.rmtree(unpacked_dir, ignore_errors=True)


def _replace_once(path: pathlib.Path, pattern: str, repl: str) -> None:
    if not path.is_file():
        return
    src = path.read_text(encoding="utf-8")
    dst, count = re.subn(pattern, repl, src, count=1, flags=re.MULTILINE)
    if count:
        path.write_text(dst, encoding="utf-8")


def sync_local_main(gh: GitHubClient, repo: pathlib.Path, dry_run: bool) -> None:
    if not AUTO_SYNC_LOCAL or dry_run:
        return
    extra, remote = git_remote_args(gh.repo_slug, gh.token)
    fetch = subprocess.run(
        ["git", *extra, "fetch", remote, "main:refs/remotes/origin/main", "--tags"],
        cwd=repo,
        capture_output=True,
        text=True,
        check=False,
    )
    if fetch.returncode != 0:
        return
    branch = subprocess.run(
        ["git", "rev-parse", "--abbrev-ref", "HEAD"],
        cwd=repo,
        capture_output=True,
        text=True,
        check=False,
    ).stdout.strip()
    if branch != "main":
        return
    dirty = subprocess.run(
        ["git", "status", "--porcelain"],
        cwd=repo,
        capture_output=True,
        text=True,
        check=False,
    ).stdout.strip()
    if dirty:
        return
    head = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=repo, capture_output=True, text=True, check=False
    ).stdout.strip()
    remote_head = subprocess.run(
        ["git", "rev-parse", "refs/remotes/origin/main"],
        cwd=repo,
        capture_output=True,
        text=True,
        check=False,
    ).stdout.strip()
    if not head or not remote_head or head == remote_head:
        return
    is_anc = subprocess.run(
        ["git", "merge-base", "--is-ancestor", head, remote_head],
        cwd=repo,
        check=False,
    )
    if is_anc.returncode == 0:
        ff = subprocess.run(
            ["git", "merge", "--ff-only", remote_head],
            cwd=repo,
            capture_output=True,
            text=True,
            check=False,
        )
        if ff.returncode == 0:
            print(f"Local main fast-forwarded: {head[:7]} -> {remote_head[:7]}")


def watch_upstream_and_open_pr(
    gh: GitHubClient,
    repo: pathlib.Path,
    state: dict[str, Any],
    dry_run: bool,
    force: bool = False,
) -> bool:
    if not AUTO_WATCH_UPSTREAM and not force:
        return False
    now = int(time.time())
    last_check = int(state.get("last_upstream_check_ts", 0))
    if not force and (now - last_check) < UPSTREAM_CHECK_INTERVAL_SEC:
        return False

    extra, remote = git_remote_args(gh.repo_slug, gh.token)
    subprocess.run(
        ["git", *extra, "fetch", remote, "main:refs/remotes/origin/main"],
        cwd=repo,
        capture_output=True,
        text=True,
        check=False,
    )
    wt_dir = pathlib.Path(tempfile.mkdtemp(prefix="zalo-rust-watch-"))
    tmp_json = pathlib.Path(tempfile.mktemp(prefix="zalo-upstream-diff-", suffix=".json"))
    try:
        subprocess.run(
            ["git", "worktree", "add", "--detach", str(wt_dir), "refs/remotes/origin/main"],
            cwd=repo,
            capture_output=True,
            text=True,
            check=True,
        )
        proc = subprocess.run(
            [
                sys.executable,
                str(wt_dir / "scripts" / "zalo-upstream-diff.py"),
                "--repo",
                str(wt_dir),
                "--download",
                "--analyze",
                "--update-protocol",
                "--report-dir",
                "docs/upstream-reports",
                "--json-out",
                str(tmp_json),
            ],
            cwd=wt_dir,
            capture_output=True,
            text=True,
            check=False,
        )
        state["last_upstream_check_ts"] = now
        if proc.returncode != 0 or not tmp_json.is_file():
            print(f"Upstream watcher check failed (rc={proc.returncode}): {(proc.stderr or proc.stdout)[-400:]}")
            return False
        summary = json.loads(tmp_json.read_text(encoding="utf-8"))
        upstream_ver = summary.get("upstream_ver") or (summary.get("probe") or {}).get("latest_version", "")
        current_ver = summary.get("current_ver", "")
        is_newer = bool(summary.get("is_newer"))
        print(f"Upstream check: pinned={current_ver}, latest={upstream_ver}, newer={is_newer}")
        if not is_newer or not upstream_ver:
            return False

        branch = f"chore/upstream-zalo-{upstream_ver}"
        existing = gh.list_open_prs_by_head(branch)
        if existing:
            print(f"Upstream PR #{existing[0]['number']} already open for {branch}; skipping.")
            return False

        if dry_run:
            print(f"[DRY-RUN] Would open PR {branch} for Zalo Desktop {upstream_ver}")
            return False

        report_path = wt_dir / "docs" / "upstream-reports" / f"zalo-{upstream_ver}.md"
        subprocess.run(["git", "checkout", "-B", branch], cwd=wt_dir, check=True)
        subprocess.run(
            ["git", "add", "protocol.json", str(report_path.relative_to(wt_dir))],
            cwd=wt_dir,
            check=True,
        )
        staged = subprocess.run(
            ["git", "diff", "--cached", "--quiet"], cwd=wt_dir, check=False
        )
        if staged.returncode == 0:
            return False
        subprocess.run(
            ["git", "commit", "-m", f"chore(upstream): analyze Zalo Desktop {upstream_ver} (from {current_ver})"],
            cwd=wt_dir,
            check=True,
        )
        subprocess.run(
            ["git", *extra, "push", "--force-with-lease", remote, f"HEAD:{branch}"],
            cwd=wt_dir,
            check=True,
        )
        for name, color, desc in AUTOMATION_LABELS:
            gh.ensure_label(name, color, desc)
        classification = summary.get("classification") or (summary.get("analysis") or {}).get("classification", "")
        release_label = (
            "release:minor" if classification == "feature-update" else "release:patch"
        )
        body = report_path.read_text(encoding="utf-8") if report_path.is_file() else ""
        pr = gh.create_pr(
            title=f"chore(upstream): sync Zalo Desktop {upstream_ver}",
            head=branch,
            base="main",
            body=body,
        )
        pr_num = int(pr["number"])
        gh.add_labels(pr_num, ["upstream-sync", "agy-auto", release_label])
        print(f"Opened upstream PR #{pr_num}: {pr.get('html_url', '')}")
        notify_desktop(
            f"Zalo Desktop có bản mới {upstream_ver}",
            f"Đã tự tạo PR #{pr_num} để agy & CodeRabbit xử lý.",
        )
        return True
    finally:
        subprocess.run(
            ["git", "worktree", "remove", "--force", str(wt_dir)],
            cwd=repo,
            check=False,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        shutil.rmtree(wt_dir, ignore_errors=True)
        tmp_json.unlink(missing_ok=True)


def bump_and_push_release_tag(
    gh: GitHubClient,
    repo: pathlib.Path,
    labels: set[str],
    state: dict[str, Any],
    dry_run: bool,
) -> None:
    if dry_run:
        print("[DRY-RUN] Would bump version and push release tag via PAT.")
        return
    extra, remote = git_remote_args(gh.repo_slug, gh.token)
    subprocess.run(
        ["git", *extra, "fetch", remote, "main:refs/remotes/origin/main", "--tags"],
        cwd=repo,
        check=True,
    )
    wt_dir = pathlib.Path(tempfile.mkdtemp(prefix="zalo-rust-rel-"))
    try:
        subprocess.run(
            ["git", "worktree", "add", "--detach", str(wt_dir), "refs/remotes/origin/main"],
            cwd=repo,
            check=True,
        )
        cargo_toml = wt_dir / "Cargo.toml"
        text = cargo_toml.read_text(encoding="utf-8")
        m = re.search(r'(?m)^version\s*=\s*"(\d+)\.(\d+)\.(\d+)"', text)
        if not m:
            print("bump_and_push_release_tag: version not found in Cargo.toml")
            return
        maj, min_, pat = int(m.group(1)), int(m.group(2)), int(m.group(3))
        if "release:major" in labels:
            maj, min_, pat = maj + 1, 0, 0
        elif "release:minor" in labels:
            min_, pat = min_ + 1, 0
        else:
            pat += 1
        new_ver = f"{maj}.{min_}.{pat}"
        tag = f"v{new_ver}"

        existing_tags = {t.get("name", "") for t in gh.list_tags(per_page=15)}
        if tag in existing_tags:
            print(f"Release tag {tag} already exists on remote; skipping local bump.")
            return

        _replace_once(wt_dir / "Cargo.toml", r'^version\s*=\s*"[^"]+"', f'version = "{new_ver}"')
        _replace_once(wt_dir / "zalo-linux/packaging/arch/PKGBUILD", r'^pkgver=.*$', f"pkgver={new_ver}")
        _replace_once(
            wt_dir / "zalo-linux/packaging/rpm/zalo-linux.spec",
            r'^Version:\s*.*$',
            f"Version:        {new_ver}",
        )
        _replace_once(
            wt_dir / "zalo-linux/packaging/deb/build-deb.sh",
            r"WORKSPACE_VERSION:-([0-9.]+)",
            f"WORKSPACE_VERSION:-{new_ver}",
        )
        _replace_once(
            wt_dir / "zalo-macos/packaging/build-app.sh",
            r"WORKSPACE_VERSION:-([0-9.]+)",
            f"WORKSPACE_VERSION:-{new_ver}",
        )
        _replace_once(
            wt_dir / "zalo-windows/packaging/build-windows.ps1",
            r'\$WorkspaceVersion\s*=\s*"[^"]+"',
            f'$WorkspaceVersion = "{new_ver}"',
        )
        subprocess.run(
            [
                "git",
                "add",
                "Cargo.toml",
                "zalo-linux/packaging/arch/PKGBUILD",
                "zalo-linux/packaging/rpm/zalo-linux.spec",
                "zalo-linux/packaging/deb/build-deb.sh",
                "zalo-macos/packaging/build-app.sh",
                "zalo-windows/packaging/build-windows.ps1",
            ],
            cwd=wt_dir,
            check=True,
        )
        subprocess.run(["git", "commit", "-m", f"chore(release): {tag}"], cwd=wt_dir, check=True)
        subprocess.run(["git", "tag", "-a", tag, "-m", f"Release {tag}"], cwd=wt_dir, check=True)
        push = subprocess.run(
            ["git", *extra, "push", remote, "HEAD:main", f"refs/tags/{tag}"],
            cwd=wt_dir,
            capture_output=True,
            text=True,
            check=False,
        )
        if push.returncode == 0:
            retagged = state.setdefault("retagged_tags", [])
            if tag not in retagged:
                retagged.append(tag)
            print(f"Pushed release commit and tag {tag} via PAT (triggering release.yml).")
            notify_desktop(
                f"Zalo-Rust đang build bản {tag}",
                "Đã tự động đẩy tag lên GitHub Actions để đóng gói Linux/macOS/Windows.",
            )
        else:
            print(f"Release bump push skipped/raced ({tag}): {push.stderr.strip()[-300:]}")
    finally:
        subprocess.run(
            ["git", "worktree", "remove", "--force", str(wt_dir)],
            cwd=repo,
            check=False,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        shutil.rmtree(wt_dir, ignore_errors=True)


def ensure_latest_tag_triggers_release(
    gh: GitHubClient,
    repo: pathlib.Path,
    state: dict[str, Any],
    dry_run: bool,
) -> None:
    """Re-push the latest v* tag using the user's PAT if GITHUB_TOKEN pushed it without triggering release.yml."""
    if not AUTO_TAG_RELEASE:
        return
    now = int(time.time())
    if (now - int(state.get("last_tag_check_ts", 0))) < 300:
        return
    state["last_tag_check_ts"] = now

    tags = [t for t in gh.list_tags(per_page=10) if re.match(r"^v\d+\.\d+\.\d+$", t.get("name", ""))]
    if not tags:
        return
    latest = tags[0]
    tag = latest["name"]
    tag_sha = (latest.get("commit") or {}).get("sha", "")
    retagged = state.setdefault("retagged_tags", [])
    if tag in retagged:
        return

    if gh.get_release_by_tag(tag) is not None:
        retagged.append(tag)
        return

    runs = gh.list_workflow_runs(per_page=25)
    for r in runs:
        if r.get("name") == "Release" and (r.get("head_branch") == tag or r.get("head_sha") == tag_sha):
            retagged.append(tag)
            return

    if dry_run:
        print(f"[DRY-RUN] Tag {tag} has no Release/workflow run; would re-push tag with PAT.")
        return

    extra, remote = git_remote_args(gh.repo_slug, gh.token)
    subprocess.run(
        ["git", *extra, "fetch", remote, f"refs/tags/{tag}:refs/tags/{tag}", "--force"],
        cwd=repo,
        check=True,
    )
    gh.delete_ref(f"tags/{tag}")
    subprocess.run(["git", *extra, "push", remote, f"refs/tags/{tag}"], cwd=repo, check=True)
    retagged.append(tag)
    print(f"Re-pushed tag {tag} via PAT to trigger GitHub Actions release.yml.")
    notify_desktop(
        f"Zalo-Rust đang build Release {tag}",
        "Đã tự động kích hoạt workflow đóng gói Linux/macOS/Windows trên GitHub.",
    )


def check_new_github_release_and_notify(
    gh: GitHubClient,
    repo: pathlib.Path,
    state: dict[str, Any],
    dry_run: bool,
) -> None:
    if not NOTIFY_RELEASES:
        return
    now = int(time.time())
    if (now - int(state.get("last_release_check_ts", 0))) < RELEASE_CHECK_INTERVAL_SEC:
        return
    state["last_release_check_ts"] = now

    rel = gh.get_latest_release()
    if not rel or rel.get("draft") or rel.get("prerelease"):
        return
    tag = rel.get("tag_name", "")
    if not tag or tag == state.get("last_notified_release"):
        return

    if not dry_run:
        url = rel.get("html_url", f"https://github.com/{gh.repo_slug}/releases/latest")
        assets_count = len(rel.get("assets") or [])
        notify_desktop(
            f"Zalo-Rust có phiên bản mới {tag}",
            f"Đã xuất bản ({assets_count} gói tải về).\n{url}",
        )
        state["last_notified_release"] = tag
    print(f"Detected published GitHub Release {tag}.")


def main() -> int:
    ap = argparse.ArgumentParser(description="Antigravity CLI autonomous PR worker for Zalo-Rust")
    ap.add_argument("--repo", type=pathlib.Path, default=REPO_ROOT)
    ap.add_argument("--repo-slug", default=os.environ.get("ZALO_GITHUB_REPO", DEFAULT_REPO_SLUG))
    ap.add_argument("--agy-bin", default=DEFAULT_AGY_BIN)
    ap.add_argument("--pr", type=int, help="Process only one specific PR number")
    ap.add_argument("--check-upstream", action="store_true", help="Force immediate upstream Zalo version check")
    ap.add_argument("--dry-run", action="store_true", help="Inspect PRs and print planned actions without modifying anything")
    ap.add_argument("--once", action="store_true", help="Run a single poll pass and exit (default)")
    args = ap.parse_args()

    token = os.environ.get("GITHUB_TOKEN") or os.environ.get("GH_TOKEN") or ""
    if not token and not args.dry_run:
        print("ERROR: GITHUB_TOKEN (or GH_TOKEN) environment variable is required.", file=sys.stderr)
        return 2
    if not os.path.isfile(args.agy_bin):
        print(f"ERROR: Antigravity CLI not found at {args.agy_bin}", file=sys.stderr)
        return 2

    gh = GitHubClient(args.repo_slug, token)
    state = load_state(DEFAULT_STATE_FILE)

    # 1. Keep local main fast-forwarded when clean.
    sync_local_main(gh, args.repo, dry_run=args.dry_run)

    # 2. Periodically check official Zalo Desktop Windows/macOS feeds and open an upstream PR if newer.
    if not args.pr:
        watch_upstream_and_open_pr(
            gh, args.repo, state, dry_run=args.dry_run, force=args.check_upstream
        )

    # 3. Process eligible open PRs with agy + CodeRabbit + CI + optional auto-merge/tag.
    try:
        prs = [gh.get_pr(args.pr)] if args.pr else gh.list_open_prs()
    except urllib.error.HTTPError as err:
        print(f"GitHub API error ({err.code}): {err.read().decode('utf-8', errors='replace')}", file=sys.stderr)
        return 1

    eligible_count = 0
    for pr in prs:
        ok, reason = is_eligible_pr(pr, args.repo_slug)
        if not ok:
            print(f"Skip PR #{pr.get('number')}: {reason}")
            continue
        eligible_count += 1
        process_pr(gh, args.repo, args.agy_bin, pr, state, dry_run=args.dry_run)

    if eligible_count == 0:
        print(f"No eligible open PRs found in {args.repo_slug} (checked {len(prs)} open PR(s)).")

    # 4. Ensure latest v* tag triggers GitHub Actions release.yml (even if created by GITHUB_TOKEN).
    if not args.pr:
        ensure_latest_tag_triggers_release(gh, args.repo, state, dry_run=args.dry_run)

    # 5. Fast-forward local main again if a merge/tag commit was just pushed, then notify on new Release.
    sync_local_main(gh, args.repo, dry_run=args.dry_run)
    if not args.pr:
        check_new_github_release_and_notify(gh, args.repo, state, dry_run=args.dry_run)

    if not args.dry_run:
        save_state(DEFAULT_STATE_FILE, state)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
