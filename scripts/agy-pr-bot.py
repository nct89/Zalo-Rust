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
AGY_TIMEOUT = os.environ.get("AGY_TIMEOUT", "30m")
AGY_EFFORT = os.environ.get("AGY_EFFORT", "high")
ELIGIBLE_LABELS = {"upstream-sync", "agy-auto"}
BLOCKING_LABELS = {"needs-human", "ready-to-merge"}


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


def inspect_pr_signals(gh: GitHubClient, pr: dict[str, Any]) -> dict[str, Any]:
    pr_num = int(pr["number"])
    head_sha = pr["head"]["sha"]
    checks = gh.list_check_runs(head_sha)
    failed_checks = []
    pending_checks = []
    for ck in checks:
        name = ck.get("name", "")
        if "coderabbit" in name.lower():
            continue
        status = ck.get("status", "")
        conclusion = ck.get("conclusion")
        if status != "completed":
            pending_checks.append(name)
        elif conclusion not in ("success", "neutral", "skipped"):
            failed_checks.append(
                {
                    "name": name,
                    "conclusion": conclusion,
                    "details_url": ck.get("html_url", ""),
                }
            )

    reviews = gh.list_reviews(pr_num)
    coderabbit_reviews = [
        r for r in reviews if "coderabbit" in ((r.get("user") or {}).get("login", "").lower())
    ]
    coderabbit_reviewed_head = any(r.get("commit_id") == head_sha for r in coderabbit_reviews)

    comments = gh.list_review_comments(pr_num)
    coderabbit_comments = [
        {
            "id": c.get("id"),
            "path": c.get("path", ""),
            "line": c.get("line") or c.get("original_line"),
            "commit_id": c.get("commit_id", ""),
            "body": c.get("body", "")[:1800],
        }
        for c in comments
        if "coderabbit" in ((c.get("user") or {}).get("login", "").lower())
        and c.get("commit_id") == head_sha
    ]

    return {
        "head_sha": head_sha,
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
) -> tuple[bool, str]:
    if dry_run:
        return True, "dry-run"
    status = subprocess.run(
        ["git", "status", "--porcelain"], cwd=worktree, capture_output=True, text=True, check=True
    ).stdout.strip()
    if not status:
        return True, "no-changes"

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
        return False, f"cargo test failed:\n{(test_proc.stdout + test_proc.stderr)[-3000:]}"

    subprocess.run(["git", "add", "-A"], cwd=worktree, check=True)
    subprocess.run(["git", "commit", "-m", commit_msg], cwd=worktree, check=True)
    extra, remote = git_remote_args(repo_slug, token)
    subprocess.run(["git", *extra, "push", remote, f"HEAD:{branch}"], cwd=worktree, check=True)
    return True, "pushed"


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
        {"rounds": 0, "implemented": False, "last_processed_sha": ""},
    )

    signals = inspect_pr_signals(gh, pr)
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
            notify_desktop(f"Zalo-Rust PR #{pr_num} đã tự Merge", f"{pr['title']} — đang chờ auto-release.")
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
        pushed_ok, push_note = verify_and_push_worktree(
            wt_dir, gh.repo_slug, gh.token, branch, msg, dry_run=False
        )
        pr_state["rounds"] += 1
        pr_state["last_processed_sha"] = head_sha
        if phase == "implement":
            pr_state["implemented"] = True

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


def main() -> int:
    ap = argparse.ArgumentParser(description="Antigravity CLI autonomous PR worker for Zalo-Rust")
    ap.add_argument("--repo", type=pathlib.Path, default=REPO_ROOT)
    ap.add_argument("--repo-slug", default=os.environ.get("ZALO_GITHUB_REPO", DEFAULT_REPO_SLUG))
    ap.add_argument("--agy-bin", default=DEFAULT_AGY_BIN)
    ap.add_argument("--pr", type=int, help="Process only one specific PR number")
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

    if not args.dry_run:
        save_state(DEFAULT_STATE_FILE, state)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
