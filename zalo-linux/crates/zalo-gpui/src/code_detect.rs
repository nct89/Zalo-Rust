//! Source-code and CLI command detection for chat message rendering.
//!
//! Splits messages containing Markdown fenced code blocks (```` ```lang ... ``` ````)
//! into prose + code segments, and detects unfenced code snippets or CLI/PowerShell
//! commands pasted directly into chat so they render cleanly in a monospace code
//! card with preserved indentation and a one-click Copy button.

/// One renderable segment of a chat message that contains source code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MessageSegment {
    /// Normal prose text.
    Text(String),
    /// Source code / command snippet with an optional display language label.
    Code {
        /// Human-readable label shown in the code block header (e.g. `PowerShell`, `Rust`, `Code`).
        lang: String,
        /// Raw code body ready to display and copy verbatim.
        code: String,
    },
}

/// Parses `text` into segments when it contains fenced code blocks or looks like
/// source code / shell commands. Returns `None` for ordinary conversational text.
#[must_use]
pub(crate) fn parse_message_segments(text: &str) -> Option<Vec<MessageSegment>> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.contains("```") {
        if let Some(segs) = parse_fenced_blocks(text) {
            return Some(segs);
        }
    }
    if let Some(lang) = detect_unfenced_code(trimmed) {
        return Some(vec![MessageSegment::Code {
            lang: lang.to_owned(),
            code: trimmed.to_owned(),
        }]);
    }
    None
}

fn parse_fenced_blocks(text: &str) -> Option<Vec<MessageSegment>> {
    let mut segments = Vec::new();
    let mut rest = text;
    let mut found_code = false;
    while let Some(open) = rest.find("```") {
        let before = &rest[..open];
        if !before.trim().is_empty() {
            segments.push(MessageSegment::Text(before.trim_matches('\n').to_owned()));
        }
        let after_open = &rest[open + 3..];
        let Some(close_rel) = after_open.find("```") else {
            // Unclosed fence: treat remainder as prose.
            if !rest[open..].trim().is_empty() {
                segments.push(MessageSegment::Text(rest[open..].trim_matches('\n').to_owned()));
            }
            rest = "";
            break;
        };
        let block = &after_open[..close_rel];
        let (lang_raw, body_raw) = if let Some(nl) = block.find('\n') {
            let first = block[..nl].trim();
            if !first.is_empty()
                && first.len() <= 24
                && first
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '#' | '-' | '_' | '.'))
            {
                (first, &block[nl + 1..])
            } else {
                ("", block)
            }
        } else {
            ("", block)
        };
        let code = body_raw.trim_matches('\n').to_owned();
        if !code.is_empty() {
            let lang = normalize_lang(lang_raw, &code);
            segments.push(MessageSegment::Code { lang, code });
            found_code = true;
        }
        rest = &after_open[close_rel + 3..];
    }
    if !rest.trim().is_empty() {
        segments.push(MessageSegment::Text(rest.trim_matches('\n').to_owned()));
    }
    found_code.then_some(segments)
}

fn normalize_lang(raw: &str, code: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" => detect_unfenced_code(code).unwrap_or("Code").to_owned(),
        "ps" | "ps1" | "powershell" | "pwsh" => "PowerShell".to_owned(),
        "sh" | "bash" | "zsh" | "shell" | "cmd" | "bat" => "Shell".to_owned(),
        "rs" | "rust" => "Rust".to_owned(),
        "js" | "javascript" | "jsx" => "JavaScript".to_owned(),
        "ts" | "typescript" | "tsx" => "TypeScript".to_owned(),
        "py" | "python" => "Python".to_owned(),
        "json" | "jsonc" => "JSON".to_owned(),
        "sql" => "SQL".to_owned(),
        "html" | "xml" => "HTML".to_owned(),
        "css" | "scss" => "CSS".to_owned(),
        "yaml" | "yml" => "YAML".to_owned(),
        "toml" => "TOML".to_owned(),
        "go" | "golang" => "Go".to_owned(),
        "c" => "C".to_owned(),
        "cpp" | "c++" | "cxx" => "C++".to_owned(),
        "cs" | "csharp" => "C#".to_owned(),
        "java" => "Java".to_owned(),
        "swift" => "Swift".to_owned(),
        "kt" | "kotlin" => "Kotlin".to_owned(),
        "php" => "PHP".to_owned(),
        other => other.to_owned(),
    }
}

/// Heuristic detection for unfenced code snippets and CLI / PowerShell commands.
#[must_use]
pub(crate) fn detect_unfenced_code(text: &str) -> Option<&'static str> {
    let trimmed = text.trim();
    if trimmed.len() < 8 {
        return None;
    }
    // Never classify bare URLs as code.
    if (trimmed.starts_with("http://") || trimmed.starts_with("https://"))
        && !trimmed.contains('\n')
        && !trimmed.contains(' ')
    {
        return None;
    }

    // 1. Valid structured JSON object or array.
    if trimmed.len() >= 16
        && ((trimmed.starts_with('{') && trimmed.ends_with('}'))
            || (trimmed.starts_with('[') && trimmed.ends_with(']')))
        && trimmed.contains(':')
        && serde_json::from_str::<serde_json::Value>(trimmed).is_ok()
    {
        return Some("JSON");
    }

    // 2. Shebang line.
    if trimmed.starts_with("#!/") {
        return Some("Shell");
    }

    // 3. PowerShell cmdlets / variables / flags.
    let mut ps_score = 0u32;
    if trimmed.contains("$env:") {
        ps_score += 3;
    }
    for kw in [
        "Get-Service",
        "Restart-Service",
        "Start-Service",
        "Stop-Service",
        "Set-Service",
        "New-Item",
        "Add-Content",
        "Set-Content",
        "Get-Content",
        "Out-Null",
        "Set-NetFirewallRule",
        "New-NetFirewallRule",
        "Get-NetFirewallRule",
        "Get-Process",
        "Stop-Process",
        "Start-Process",
        "Invoke-WebRequest",
        "Invoke-RestMethod",
        "Start-ScheduledTask",
        "Register-ScheduledTask",
        "Import-Module",
        "Write-Host",
        "Write-Output",
    ] {
        if trimmed.contains(kw) {
            ps_score += 3;
        }
    }
    for flag in [
        "-ItemType ",
        "-Path ",
        "-Value ",
        "-Encoding ",
        "-Profile ",
        "-Enabled ",
        "-Force",
        "-NoProfile",
        "-ExecutionPolicy",
        "icacls ",
    ] {
        if trimmed.contains(flag) {
            ps_score += 2;
        }
    }
    if has_ps_var_assignment(trimmed) {
        ps_score += 2;
    }
    if ps_score >= 3 {
        return Some("PowerShell");
    }

    // 4. Shell / CLI commands (single-line or multi-line).
    let lines: Vec<&str> = trimmed.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let cli_prefixes = [
        "winget ",
        "cargo ",
        "rustc ",
        "rustup ",
        "git ",
        "docker ",
        "kubectl ",
        "npm ",
        "npx ",
        "pnpm ",
        "yarn ",
        "pip ",
        "pip3 ",
        "python ",
        "python3 ",
        "curl ",
        "wget ",
        "ssh ",
        "scp ",
        "rsync ",
        "sudo ",
        "apt ",
        "apt-get ",
        "dnf ",
        "pacman ",
        "brew ",
        "systemctl ",
        "chmod ",
        "chown ",
        "mkdir ",
        "rm -",
        "cp -",
        "mv ",
        "grep ",
        "rg ",
        "find ",
        "netstat ",
        "findstr ",
        "export ",
        "set ",
        "cmake ",
        "make ",
        "go ",
        "dotnet ",
    ];
    let cli_line_hits = lines
        .iter()
        .filter(|line| {
            let l = line.strip_prefix("$ ").unwrap_or(line);
            cli_prefixes.iter().any(|p| l.starts_with(p))
        })
        .count();
    let has_cli_syntax = trimmed.contains(" --")
        || trimmed.contains(" | ")
        || trimmed.contains(" && ")
        || trimmed.contains("2>&1")
        || trimmed.contains("~/.")
        || trimmed.contains("C:\\");
    if cli_line_hits >= 1 && (has_cli_syntax || cli_line_hits >= 2 || lines.len() == 1 && is_pure_cli_line(lines[0])) {
        return Some("Shell");
    }

    // 5. Programming languages (require strong unambiguous language constructs).
    let mut rust_score = 0u32;
    for tok in [
        "fn ", "pub fn ", "let mut ", "impl ", "struct ", "enum ", "println!", "use std::",
        "#[derive", "-> Result<", "-> Option<", "match ",
    ] {
        if trimmed.contains(tok) {
            rust_score += 2;
        }
    }
    if trimmed.contains("::") && (trimmed.contains('{') || trimmed.contains(';')) {
        rust_score += 2;
    }
    if rust_score >= 4 {
        return Some("Rust");
    }

    let mut js_score = 0u32;
    for tok in [
        "const ",
        "let ",
        "function ",
        "console.log(",
        "import ",
        "export default",
        "export function",
        "async function",
        "=> {",
        "interface ",
        "document.querySelector",
    ] {
        if trimmed.contains(tok) {
            js_score += 2;
        }
    }
    if js_score >= 4 && (trimmed.contains('{') || trimmed.contains(';') || trimmed.contains("=>")) {
        return Some("JavaScript");
    }

    let mut py_score = 0u32;
    for tok in [
        "def ",
        "class ",
        "import ",
        "if __name__ ==",
        "print(",
        "elif ",
        "return ",
    ] {
        if trimmed.contains(tok) {
            py_score += 2;
        }
    }
    if py_score >= 4 && (trimmed.contains(':') && lines.len() >= 2) {
        return Some("Python");
    }

    let upper = trimmed.to_ascii_uppercase();
    if (upper.starts_with("SELECT ") && upper.contains(" FROM "))
        || upper.starts_with("INSERT INTO ")
        || (upper.starts_with("UPDATE ") && upper.contains(" SET "))
        || upper.starts_with("DELETE FROM ")
        || upper.starts_with("CREATE TABLE ")
        || upper.starts_with("ALTER TABLE ")
    {
        return Some("SQL");
    }

    if lines.len() >= 2
        && (trimmed.contains("</div")
            || trimmed.contains("</html")
            || trimmed.contains("</script")
            || trimmed.contains("</style")
            || trimmed.contains("<!DOCTYPE"))
    {
        return Some("HTML");
    }

    // 6. Generic multi-line code fallback (requires >= 3 lines + strong code density).
    if lines.len() >= 3 {
        let mut signals = 0u32;
        let semi_lines = lines.iter().filter(|l| l.ends_with(';')).count();
        let brace_lines = lines
            .iter()
            .filter(|l| l.ends_with('{') || **l == "}" || l.starts_with("} "))
            .count();
        let indented_lines = trimmed
            .lines()
            .filter(|l| l.starts_with("  ") || l.starts_with('\t'))
            .count();
        if semi_lines >= 2 {
            signals += 2;
        }
        if brace_lines >= 2 {
            signals += 2;
        }
        if indented_lines >= 2 {
            signals += 1;
        }
        if trimmed.contains("==") || trimmed.contains("!=") || trimmed.contains("->") || trimmed.contains(":=") {
            signals += 1;
        }
        if signals >= 4 {
            return Some("Code");
        }
    }

    None
}

fn has_ps_var_assignment(text: &str) -> bool {
    let bytes = text.as_bytes();
    for i in 0..bytes.len().saturating_sub(2) {
        if bytes[i] == b'$' && bytes[i + 1].is_ascii_alphabetic() {
            if let Some(rest) = text[i + 1..].find('=') {
                if rest <= 24
                    && text[i + 1..i + 1 + rest]
                        .trim()
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_')
                {
                    return true;
                }
            }
        }
    }
    false
}

fn is_pure_cli_line(line: &str) -> bool {
    let l = line.strip_prefix("$ ").unwrap_or(line).trim();
    // Single-line package/tool install commands like `winget install Rustlang.Rustup`
    // have no Vietnamese diacritics and are compact ASCII command tokens.
    let words: Vec<&str> = l.split_whitespace().collect();
    words.len() >= 2
        && words.len() <= 14
        && l.is_ascii()
        && !l.ends_with('?')
        && !l.ends_with('!')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fenced_code_blocks_with_surrounding_text() {
        let msg = "Chạy lệnh này:\n```rust\nfn main() {\n    println!(\"hi\");\n}\n```\nXong rồi!";
        let segs = parse_message_segments(msg).expect("should parse fenced");
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0], MessageSegment::Text("Chạy lệnh này:".into()));
        assert_eq!(
            segs[1],
            MessageSegment::Code {
                lang: "Rust".into(),
                code: "fn main() {\n    println!(\"hi\");\n}".into(),
            }
        );
        assert_eq!(segs[2], MessageSegment::Text("Xong rồi!".into()));
    }

    #[test]
    fn detects_powershell_and_cli_snippets_from_user_screenshots() {
        let ps1 = "$userSsh = \"$env:USERPROFILE\\.ssh\"\nNew-Item -ItemType Directory -Force -Path $userSsh | Out-Null\nRestart-Service sshd";
        assert_eq!(detect_unfenced_code(ps1), Some("PowerShell"));

        let ps2 = "$env:USERNAME\nrustc --version\ncargo --version";
        assert_eq!(detect_unfenced_code(ps2), Some("PowerShell"));

        let ps3 = "Get-Service sshd\nnetstat -ano | findstr :22\nSet-NetFirewallRule -Name \"OpenSSH-Server-In-TCP\" -Profile Any -Enabled True";
        assert_eq!(detect_unfenced_code(ps3), Some("PowerShell"));

        let winget = "winget install Rustlang.Rustup";
        assert_eq!(detect_unfenced_code(winget), Some("Shell"));
    }

    #[test]
    fn ignores_normal_vietnamese_messages_and_urls() {
        assert_eq!(
            parse_message_segments("Em đi sớm 30p nha ghi nhận dùm em @Trường"),
            None
        );
        assert_eq!(
            parse_message_segments("@Trường kiểm tra kỹ các bạn nào đi trễ nghỉ lố ngày."),
            None
        );
        assert_eq!(parse_message_segments("https://chat.zalo.me"), None);
        assert_eq!(parse_message_segments("ok cảm ơn anh nhiều"), None);
    }
}
