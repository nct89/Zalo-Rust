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

## 3. Antigravity CLI (`agy`) PR Worker (`agy-pr-bot.py`)

Daemon cục bộ trên máy Linux nơi bạn đã đăng nhập sẵn Antigravity CLI (`/home/nct89/.local/bin/agy`).

### Cài đặt 1 lần

1. Tạo file biến môi trường `~/.config/zalo-rust-bot/env` (chmod `600`):
   ```bash
   mkdir -p ~/.config/zalo-rust-bot
   cat > ~/.config/zalo-rust-bot/env <<'EOF'
   GITHUB_TOKEN=github_pat_...
   AGY_BIN=/home/nct89/.local/bin/agy
   AUTO_MERGE=false
   MAX_AGY_ROUNDS=3
   AGY_EFFORT=high
   AGY_TIMEOUT=30m
   EOF
   chmod 600 ~/.config/zalo-rust-bot/env
   ```
   *(Để tự động tạo tag và kích hoạt `release.yml` sau khi PR merge, thêm cùng PAT có quyền `Contents: Read & Write` vào GitHub Repository Secret tên `RELEASE_TOKEN`.)*

2. Chạy thử dry-run:
   ```bash
   set -a && source ~/.config/zalo-rust-bot/env && set +a
   python3 scripts/agy-pr-bot.py --dry-run
   ```

3. Bật chạy tự động mỗi 10 phút qua `systemd --user`:
   ```bash
   mkdir -p ~/.config/systemd/user
   cp scripts/systemd/zalo-agy-pr-bot.service ~/.config/systemd/user/
   cp scripts/systemd/zalo-agy-pr-bot.timer ~/.config/systemd/user/
   systemctl --user daemon-reload
   systemctl --user enable --now zalo-agy-pr-bot.timer
   systemctl --user status zalo-agy-pr-bot.timer
   ```
