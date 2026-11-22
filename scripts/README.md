# Automation Scripts (`Zalo-Rust`)

## 1. Upstream Zalo Desktop Analyzer (`zalo-upstream-diff.py`)

Kiểm tra phiên bản Zalo Desktop chính thức cho cả **Windows** và **macOS**, tự tải/giải nén `app.asar` (dùng `7z` + parser ASAR thuần Python) và xuất báo cáo so sánh với `Zalo-Rust`:

```bash
# Chỉ kiểm tra phiên bản mới nhất
python3 scripts/zalo-upstream-diff.py --check-only

# Tải, giải nén và xuất báo cáo vào docs/upstream-reports/
python3 scripts/zalo-upstream-diff.py --download --analyze --report-dir docs/upstream-reports
```

Workflow `.github/workflows/zalo-watcher.yml` tự động chạy script này hằng ngày (09:00 UTC+7) và tự mở Pull Request gắn nhãn `upstream-sync` + `agy-auto` + `release:patch`/`release:minor`.

---

## 2. CodeRabbit Automated Review (`.coderabbit.yaml`)

1. Cài **CodeRabbit GitHub App** cho repo `nct89/Zalo-Rust` tại https://github.com/apps/coderabbitai.
2. Mọi PR mở vào `main` sẽ được CodeRabbit tự động review bằng Tiếng Việt theo luật trong `.coderabbit.yaml` (và tự re-review khi có commit mới).

---

## 3. Antigravity CLI (`agy`) Full Zero-Touch Worker (`agy-pr-bot.py`)

Daemon cục bộ trên máy Linux nơi bạn đã đăng nhập sẵn Antigravity CLI (`/home/nct89/.local/bin/agy`). Khi bật đủ cờ, daemon tự động hóa **100% toàn trình** ngay trên máy bạn:

1. **Đồng bộ `main` local (`AUTO_SYNC_LOCAL=true`)**: tự `git fetch --tags` và `git merge --ff-only` khi nhánh `main` local sạch.
2. **Dò bản Zalo Desktop mới (`AUTO_WATCH_UPSTREAM=true`)**: định kỳ mỗi 6 giờ (`UPSTREAM_CHECK_INTERVAL_SEC=21600`) chạy `zalo-upstream-diff.py` trên worktree sạch, thấy bản Windows/macOS mới là tự mở PR `chore/upstream-zalo-<ver>`.
3. **Tự code & sửa review/CI bằng `agy`**: gọi `agy` trên worktree riêng, tự đọc log lỗi CI & góp ý CodeRabbit, chạy `cargo test --workspace`, commit & push.
4. **Tự động Merge & dọn nhánh (`AUTO_MERGE=true`, `DELETE_MERGED_BRANCH=true`)**: khi toàn bộ CI xanh và CodeRabbit sạch, bot tự squash-merge PR và xóa nhánh tạm.
5. **Tự nâng version & đẩy tag kích hoạt `release.yml` (`AUTO_TAG_RELEASE=true`)**: tự bump `Cargo.toml` + 5 file đóng gói và đẩy tag `vX.Y.Z` bằng chính PAT (tự động kích hoạt GitHub Actions build gói Linux/macOS/Windows và đăng GitHub Release mà không cần thêm `RELEASE_TOKEN` trên GitHub).
6. **Thông báo bản phát hành mới (`NOTIFY_RELEASES=true`)**: kiểm tra `releases/latest` định kỳ và gửi desktop notification (`notify-send`) khi có Release mới.

### Cài đặt 1 lần

1. Tạo file biến môi trường `~/.config/zalo-rust-bot/env` (chmod `600`):
   ```bash
   mkdir -p ~/.config/zalo-rust-bot
   cat > ~/.config/zalo-rust-bot/env <<'EOF'
   GITHUB_TOKEN=github_pat_...
   AGY_BIN=/home/nct89/.local/bin/agy
   AUTO_MERGE=true
   AUTO_WATCH_UPSTREAM=true
   AUTO_TAG_RELEASE=true
   AUTO_SYNC_LOCAL=true
   NOTIFY_RELEASES=true
   DELETE_MERGED_BRANCH=true
   UPSTREAM_CHECK_INTERVAL_SEC=21600
   RELEASE_CHECK_INTERVAL_SEC=900
   MAX_AGY_ROUNDS=3
   AGY_EFFORT=high
   AGY_TIMEOUT=30m
   EOF
   chmod 600 ~/.config/zalo-rust-bot/env
   ```

2. Chạy thử dry-run:
   ```bash
   set -a && source ~/.config/zalo-rust-bot/env && set +a
   python3 scripts/agy-pr-bot.py --dry-run
   ```

3. Bật chạy tự động mỗi 3 phút qua `systemd --user`:
   ```bash
   mkdir -p ~/.config/systemd/user
   cp scripts/systemd/zalo-agy-pr-bot.service ~/.config/systemd/user/
   cp scripts/systemd/zalo-agy-pr-bot.timer ~/.config/systemd/user/
   systemctl --user daemon-reload
   systemctl --user enable --now zalo-agy-pr-bot.timer
   systemctl --user status zalo-agy-pr-bot.timer
   ```
