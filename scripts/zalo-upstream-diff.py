#!/usr/bin/env python3
"""Official Zalo Desktop (Windows & macOS) upstream detector, unpacker, and diff analyzer.

Probes the live official Zalo download redirector (`https://zalo.me/download/zalo-pc`)
for both Windows (`.exe`) and macOS (`.dmg`), falls back to Homebrew Cask API for macOS,
optionally downloads & unpacks `app.asar` (pure Python ASAR reader + `7z`), and emits a
structured JSON + Markdown diff report against the current `Zalo-Rust` repository.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request
from typing import Any

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
ZALO_DOWNLOAD_URL = "https://zalo.me/download/zalo-pc"
BREW_CASK_API_URL = "https://formulae.brew.sh/api/cask/zalo.json"

WIN_UA = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36"
MAC_UA = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36"

VERSION_RE = re.compile(r"(\d+\.\d+\.\d+)")


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def parse_version_tuple(ver: str) -> tuple[int, ...]:
    nums = [int(x) for x in re.findall(r"\d+", ver)]
    return tuple(nums) if nums else (0,)


def probe_redirect(user_agent: str, timeout: int = 20) -> dict[str, str]:
    opener = urllib.request.build_opener(_NoRedirect)
    req = urllib.request.Request(ZALO_DOWNLOAD_URL, headers={"User-Agent": user_agent})
    location = ""
    try:
        resp = opener.open(req, timeout=timeout)
        location = resp.headers.get("Location", "")
    except urllib.error.HTTPError as err:
        if err.code in (301, 302, 303, 307, 308):
            location = err.headers.get("Location", "")
        else:
            raise
    m = VERSION_RE.search(location)
    return {
        "version": m.group(1) if m else "",
        "url": location,
    }


def probe_brew_cask(timeout: int = 20) -> dict[str, str]:
    req = urllib.request.Request(BREW_CASK_API_URL, headers={"User-Agent": "Zalo-Rust-Watcher/2.0"})
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        data = json.loads(resp.read().decode("utf-8", errors="replace"))
    return {
        "version": str(data.get("version", "")).strip(),
        "url": str(data.get("url", "")).strip(),
    }


def probe_upstream() -> dict[str, Any]:
    win = {"version": "", "url": "", "error": ""}
    mac = {"version": "", "url": "", "error": ""}

    try:
        res = probe_redirect(WIN_UA)
        win.update(res)
    except Exception as exc:
        win["error"] = str(exc)

    try:
        res = probe_redirect(MAC_UA)
        mac.update(res)
    except Exception as exc:
        mac["error"] = str(exc)

    if not mac["version"]:
        try:
            res = probe_brew_cask()
            mac.update(res)
        except Exception as exc:
            mac["error"] = (mac["error"] + "; " if mac["error"] else "") + str(exc)

    best_platform = ""
    best_ver = ""
    best_url = ""
    candidates = []
    if win["version"]:
        candidates.append((parse_version_tuple(win["version"]), 1, "windows", win["version"], win["url"]))
    if mac["version"]:
        candidates.append((parse_version_tuple(mac["version"]), 0, "macos", mac["version"], mac["url"]))
    if candidates:
        candidates.sort(reverse=True)
        _, _, best_platform, best_ver, best_url = candidates[0]

    return {
        "windows": win,
        "macos": mac,
        "latest_platform": best_platform,
        "latest_version": best_ver,
        "latest_url": best_url,
    }


def download_file(url: str, dst: pathlib.Path) -> pathlib.Path:
    dst.parent.mkdir(parents=True, exist_ok=True)
    req = urllib.request.Request(url, headers={"User-Agent": WIN_UA})
    with urllib.request.urlopen(req, timeout=180) as resp, open(dst, "wb") as out:
        shutil.copyfileobj(resp, out, length=1024 * 256)
    return dst


def find_7z() -> str:
    for candidate in ("7z", "7zz", "/home/nct89/.local/bin/7z", "/usr/bin/7z"):
        found = shutil.which(candidate) if "/" not in candidate else (candidate if os.path.isfile(candidate) else None)
        if found:
            return found
    raise RuntimeError("7z/7zz not found in PATH")


def extract_installer(installer_path: pathlib.Path, out_dir: pathlib.Path) -> pathlib.Path:
    seven_z = find_7z()
    out_dir.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        [seven_z, "x", "-y", f"-o{out_dir}", str(installer_path)],
        check=False,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    # Windows NSIS / macOS DMG may nest a second archive (e.g. app-64.7z or HFS/APFS image).
    for nested_glob in ("**/*.7z", "**/*.hfs", "**/*.img"):
        for nested in out_dir.glob(nested_glob):
            if nested.is_file() and nested.stat().st_size > 1_000_000:
                sub = nested.parent / f"{nested.stem}_unpacked"
                sub.mkdir(parents=True, exist_ok=True)
                subprocess.run(
                    [seven_z, "x", "-y", f"-o{sub}", str(nested)],
                    check=False,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                )
    return out_dir


def extract_asar(asar_path: pathlib.Path, out_dir: pathlib.Path) -> int:
    """Pure-Python Electron ASAR extractor (no Node.js/npm required)."""
    with open(asar_path, "rb") as f:
        header_meta = f.read(16)
        if len(header_meta) < 16:
            raise ValueError("Truncated ASAR header")
        _, header_size, _, json_size = struct.unpack("<IIII", header_meta)
        header_json = f.read(json_size).decode("utf-8")
        data_offset = 8 + header_size
        tree = json.loads(header_json)

        extracted = 0

        def walk(node: dict[str, Any], rel: pathlib.Path) -> None:
            nonlocal extracted
            files = node.get("files")
            if isinstance(files, dict):
                (out_dir / rel).mkdir(parents=True, exist_ok=True)
                for name, child in files.items():
                    if name in (".", "..") or "/" in name or "\\" in name:
                        continue
                    walk(child, rel / name)
                return
            if node.get("unpacked"):
                return
            offset = node.get("offset")
            size = node.get("size")
            if offset is None or size is None:
                return
            target = out_dir / rel
            target.parent.mkdir(parents=True, exist_ok=True)
            f.seek(data_offset + int(offset))
            remaining = int(size)
            with open(target, "wb") as dst:
                while remaining > 0:
                    chunk = f.read(min(remaining, 1024 * 256))
                    if not chunk:
                        break
                    dst.write(chunk)
                    remaining -= len(chunk)
            extracted += 1

        walk(tree, pathlib.Path("."))
        return extracted


def sha256_file(path: pathlib.Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1024 * 256), b""):
            h.update(chunk)
    return h.hexdigest()


def collect_repo_baseline(repo: pathlib.Path) -> dict[str, Any]:
    protocol_path = repo / "protocol.json"
    protocol = json.loads(protocol_path.read_text(encoding="utf-8")) if protocol_path.is_file() else {}

    wasm_dir = repo / "assets" / "wasm"
    wasm_files: dict[str, str] = {}
    if wasm_dir.is_dir():
        for p in sorted(wasm_dir.glob("*.wasm")):
            wasm_files[p.name] = sha256_file(p)

    known_endpoints: set[str] = set()
    client_src = repo / "core" / "zalo-client" / "src"
    if client_src.is_dir():
        ep_re = re.compile(r"/api/[a-zA-Z0-9_./-]+")
        for rs in client_src.rglob("*.rs"):
            text = rs.read_text(encoding="utf-8", errors="ignore")
            known_endpoints.update(ep_re.findall(text))

    known_str_keys: set[str] = set()
    design_src = repo / "core" / "zalo-design" / "src"
    if design_src.is_dir():
        str_re = re.compile(r"\bSTR_[A-Z0-9_]+\b")
        for rs in design_src.rglob("*.rs"):
            text = rs.read_text(encoding="utf-8", errors="ignore")
            known_str_keys.update(str_re.findall(text))

    return {
        "protocol": protocol,
        "wasm_files": wasm_files,
        "known_endpoints": known_endpoints,
        "known_str_keys": known_str_keys,
    }


def analyze_unpacked(unpacked_root: pathlib.Path, baseline: dict[str, Any]) -> dict[str, Any]:
    # Extract any app.asar found under unpacked_root
    asar_extracted_dirs: list[pathlib.Path] = []
    for asar in unpacked_root.rglob("app.asar"):
        if asar.is_file():
            dst = asar.parent / "app_asar_extracted"
            try:
                count = extract_asar(asar, dst)
                if count > 0:
                    asar_extracted_dirs.append(dst)
            except Exception as exc:
                print(f"warn: failed to extract {asar}: {exc}", file=sys.stderr)

    # Scan WASM binaries
    upstream_wasm: dict[str, str] = {}
    for w in unpacked_root.rglob("libzproto*.wasm"):
        if w.is_file():
            upstream_wasm[w.name] = sha256_file(w)

    wasm_changed = upstream_wasm != baseline["wasm_files"] and bool(upstream_wasm)

    # Scan JS bundles for zpw_ver / client_version, endpoints, and new STR_* keys
    zpw_candidates: dict[str, int] = {}
    upstream_endpoints: set[str] = set()
    upstream_str_keys: set[str] = set()
    js_files_scanned = 0

    zpw_re = re.compile(r"(?:zpw_ver|CLIENT_VERSION|client_version)\s*[:=]\s*['\"]?(\d{3,4})['\"]?")
    ep_re = re.compile(r"/api/[a-zA-Z0-9_./-]{3,80}")
    str_re = re.compile(r"\bSTR_[A-Z0-9_]{3,80}\b")

    for js in unpacked_root.rglob("*.js"):
        if not js.is_file() or js.stat().st_size > 25_000_000:
            continue
        js_files_scanned += 1
        text = js.read_text(encoding="utf-8", errors="ignore")
        for m in zpw_re.findall(text):
            zpw_candidates[m] = zpw_candidates.get(m, 0) + 1
        upstream_endpoints.update(ep_re.findall(text))

    for jf in unpacked_root.rglob("*.json"):
        name_l = jf.name.lower()
        if not jf.is_file() or jf.stat().st_size > 15_000_000:
            continue
        if "lang" in name_l or "vi" in name_l or "en" in name_l:
            text = jf.read_text(encoding="utf-8", errors="ignore")
            upstream_str_keys.update(str_re.findall(text))

    detected_zpw = ""
    if zpw_candidates:
        detected_zpw = max(zpw_candidates.items(), key=lambda kv: (kv[1], int(kv[0])))[0]

    new_endpoints = sorted(upstream_endpoints - baseline["known_endpoints"])
    new_str_keys = sorted(upstream_str_keys - baseline["known_str_keys"])

    pinned_zpw = str(baseline["protocol"].get("client_version", "")).strip()
    zpw_changed = bool(detected_zpw) and detected_zpw != pinned_zpw
    has_feature_changes = wasm_changed or len(new_str_keys) > 0 or len(new_endpoints) > 0

    return {
        "asar_extracted_count": len(asar_extracted_dirs),
        "js_files_scanned": js_files_scanned,
        "detected_zpw_ver": detected_zpw,
        "pinned_zpw_ver": pinned_zpw,
        "zpw_changed": zpw_changed,
        "upstream_wasm": upstream_wasm,
        "baseline_wasm": baseline["wasm_files"],
        "wasm_changed": wasm_changed,
        "new_endpoints_count": len(new_endpoints),
        "new_endpoints_sample": new_endpoints[:40],
        "new_str_keys_count": len(new_str_keys),
        "new_str_keys_sample": new_str_keys[:60],
        "classification": "feature-update" if has_feature_changes else "protocol-only",
    }


def render_markdown_report(probe: dict[str, Any], analysis: dict[str, Any] | None, current_ver: str) -> str:
    latest = probe["latest_version"]
    lines = [
        f"# Upstream Zalo Desktop Analysis: `{latest}`",
        "",
        f"- **Pinned in `protocol.json`**: `{current_ver}`",
        f"- **Windows latest**: `{probe['windows']['version'] or 'N/A'}` ({probe['windows']['url'] or probe['windows']['error']})",
        f"- **macOS latest**: `{probe['macos']['version'] or 'N/A'}` ({probe['macos']['url'] or probe['macos']['error']})",
        f"- **Analyzed platform**: `{probe['latest_platform']}` (`{probe['latest_url']}`)",
        "",
    ]
    if analysis:
        lines += [
            "## Summary",
            "",
            f"- **Classification**: `{analysis['classification']}`",
            f"- **`client_version` (`zpw_ver`)**: pinned `{analysis['pinned_zpw_ver']}` → detected `{analysis['detected_zpw_ver'] or 'unchanged/not-found'}` (changed: `{analysis['zpw_changed']}`)",
            f"- **WASM crypto modules changed**: `{analysis['wasm_changed']}`",
            f"- **New `/api/...` endpoints not in `core/zalo-client`**: `{analysis['new_endpoints_count']}`",
            f"- **New `STR_*` UI keys not in `core/zalo-design`**: `{analysis['new_str_keys_count']}`",
            f"- **Bundles scanned**: `{analysis['asar_extracted_count']}` ASAR archive(s), `{analysis['js_files_scanned']}` JS file(s)",
            "",
            "## WASM Crypto Modules",
            "",
        ]
        if analysis["upstream_wasm"]:
            for name, digest in sorted(analysis["upstream_wasm"].items()):
                status = "MATCH" if analysis["baseline_wasm"].get(name) == digest else "NEW/CHANGED"
                lines.append(f"- `{name}` (`sha256: {digest[:16]}...`) — **{status}**")
        else:
            lines.append("- *(No `libzproto*.wasm` found in extracted bundle)*")

        if analysis["new_endpoints_sample"]:
            lines += ["", "## New API Endpoints (sample)", ""]
            for ep in analysis["new_endpoints_sample"]:
                lines.append(f"- `{ep}`")

        if analysis["new_str_keys_sample"]:
            lines += ["", "## New UI String Keys `STR_*` (sample — indicates new features)", ""]
            for key in analysis["new_str_keys_sample"]:
                lines.append(f"- `{key}`")

    lines += [
        "",
        "## Verification Checklist",
        "",
        "- [ ] Verify `protocol.json` (`zalo_desktop_version`, `client_version`, `user_agent`) and `core/zalo-protocol/src/param_cipher.rs`.",
        "- [ ] Verify `assets/wasm/libzproto_*.wasm` hashes & exports in `core/zalo-sync` and `core/zalo-backup`.",
        "- [ ] Review new API endpoints and `STR_*` keys to port any high-value features to `core/zalo-client` & `zalo-linux/crates/zalo-gpui`.",
        "- [ ] Keep `README.md` synchronized with any new user-facing features or configuration changes.",
        "- [ ] Pass `cargo test --workspace -- --test-threads=1` across Linux, macOS, and Windows CI.",
        "",
    ]
    return "\n".join(lines)


def update_protocol_json(repo: pathlib.Path, new_ver: str, detected_zpw: str) -> bool:
    path = repo / "protocol.json"
    data = json.loads(path.read_text(encoding="utf-8"))
    changed = False
    if new_ver and data.get("zalo_desktop_version") != new_ver:
        old_ver = data.get("zalo_desktop_version", "")
        data["zalo_desktop_version"] = new_ver
        ua = data.get("user_agent", "")
        if old_ver and f"Zalo/{old_ver}" in ua:
            data["user_agent"] = ua.replace(f"Zalo/{old_ver}", f"Zalo/{new_ver}")
        changed = True
    if detected_zpw and detected_zpw.isdigit() and data.get("client_version") != detected_zpw:
        data["client_version"] = detected_zpw
        changed = True
    if changed:
        path.write_text(json.dumps(data, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    return changed


def write_github_output(kv: dict[str, str]) -> None:
    out_path = os.environ.get("GITHUB_OUTPUT")
    if not out_path:
        return
    with open(out_path, "a", encoding="utf-8") as f:
        for k, v in kv.items():
            f.write(f"{k}={v}\n")


def main() -> int:
    ap = argparse.ArgumentParser(description="Check, unpack, and diff upstream Zalo Desktop releases.")
    ap.add_argument("--repo", type=pathlib.Path, default=REPO_ROOT, help="Path to Zalo-Rust repository root")
    ap.add_argument("--check-only", action="store_true", help="Only probe live versions without downloading")
    ap.add_argument("--download", action="store_true", help="Download latest installer when newer (or with --force)")
    ap.add_argument("--analyze", action="store_true", help="Unpack & analyze downloaded installer")
    ap.add_argument("--force", action="store_true", help="Proceed with download/analysis even if version is not newer")
    ap.add_argument("--update-protocol", action="store_true", help="Update protocol.json with detected version/zpw_ver")
    ap.add_argument("--report-dir", type=pathlib.Path, help="Directory to write Markdown report (zalo-<ver>.md)")
    ap.add_argument("--json-out", type=pathlib.Path, help="Optional JSON output path")
    ap.add_argument("--keep-unpacked", type=pathlib.Path, help="Persist unpacked upstream files here (for agy --add-dir)")
    args = ap.parse_args()

    baseline = collect_repo_baseline(args.repo)
    current_ver = str(baseline["protocol"].get("zalo_desktop_version", "0.0.0")).strip()
    current_zpw = str(baseline["protocol"].get("client_version", "")).strip()

    probe = probe_upstream()
    latest_ver = probe["latest_version"]
    is_newer = bool(latest_ver) and parse_version_tuple(latest_ver) > parse_version_tuple(current_ver)

    print(
        f"Pinned: {current_ver} (zpw_ver={current_zpw}) | "
        f"Win: {probe['windows']['version'] or 'N/A'} | "
        f"Mac: {probe['macos']['version'] or 'N/A'} | "
        f"Latest: {latest_ver or 'unknown'} ({probe['latest_platform']}) | Newer: {is_newer}"
    )

    analysis: dict[str, Any] | None = None
    report_path = ""
    if not args.check_only and (is_newer or args.force) and probe["latest_url"] and (args.download or args.analyze):
        tmp_obj = None
        if args.keep_unpacked:
            work_dir = args.keep_unpacked
            work_dir.mkdir(parents=True, exist_ok=True)
        else:
            tmp_obj = tempfile.TemporaryDirectory(prefix="zalo-upstream-")
            work_dir = pathlib.Path(tmp_obj.name)
        try:
            ext = ".dmg" if probe["latest_url"].lower().endswith(".dmg") else ".exe"
            installer = work_dir / f"ZaloSetup-{latest_ver}{ext}"
            print(f"Downloading {probe['latest_url']} -> {installer} ...")
            download_file(probe["latest_url"], installer)
            unpacked = work_dir / "unpacked"
            print(f"Extracting installer into {unpacked} ...")
            extract_installer(installer, unpacked)
            if args.analyze:
                analysis = analyze_unpacked(unpacked, baseline)
                print(
                    f"Analysis: classification={analysis['classification']} "
                    f"zpw={analysis['detected_zpw_ver'] or 'N/A'} "
                    f"wasm_changed={analysis['wasm_changed']} "
                    f"new_endpoints={analysis['new_endpoints_count']} "
                    f"new_str_keys={analysis['new_str_keys_count']}"
                )
        finally:
            if tmp_obj is not None:
                tmp_obj.cleanup()

    if args.update_protocol and (is_newer or args.force) and latest_ver:
        detected_zpw = analysis["detected_zpw_ver"] if analysis else ""
        updated = update_protocol_json(args.repo, latest_ver, detected_zpw)
        print(f"protocol.json updated: {updated}")

    if args.report_dir and (is_newer or args.force) and latest_ver:
        args.report_dir.mkdir(parents=True, exist_ok=True)
        md_file = args.report_dir / f"zalo-{latest_ver}.md"
        md_file.write_text(render_markdown_report(probe, analysis, current_ver), encoding="utf-8")
        report_path = str(md_file.relative_to(args.repo) if md_file.is_relative_to(args.repo) else md_file)
        print(f"Wrote report: {md_file}")

    result = {
        "current_ver": current_ver,
        "current_zpw": current_zpw,
        "is_newer": is_newer,
        "probe": probe,
        "analysis": analysis,
        "report_path": report_path,
    }
    if args.json_out:
        args.json_out.parent.mkdir(parents=True, exist_ok=True)
        args.json_out.write_text(json.dumps(result, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")

    write_github_output(
        {
            "current_ver": current_ver,
            "current_zpw": current_zpw,
            "upstream_ver": latest_ver,
            "upstream_platform": probe["latest_platform"],
            "upstream_url": probe["latest_url"],
            "win_ver": probe["windows"]["version"],
            "mac_ver": probe["macos"]["version"],
            "is_newer": "true" if is_newer else "false",
            "classification": (analysis or {}).get("classification", "unknown"),
            "report_path": report_path,
        }
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
