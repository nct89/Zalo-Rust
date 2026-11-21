//! Pick an image, any file, or a folder: XDG portal `FileChooser.OpenFile`,
//! then `zenity` or `kdialog` when the portal is not available.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use zbus::zvariant::{OwnedValue, Value};

use crate::bus::session;
use crate::error::ShellError;
use crate::screenshot::{
    parse_file_uri, portal_request_path, PORTAL_PATH, PORTAL_REQUEST_INTERFACE, PORTAL_SERVICE,
};

/// Portal interface for file dialogs.
pub const PORTAL_FILECHOOSER_INTERFACE: &str = "org.freedesktop.portal.FileChooser";

/// Glob patterns offered by the image picker.
pub const IMAGE_PATTERNS: &[&str] = &[
    "*.png", "*.jpg", "*.jpeg", "*.webp", "*.gif", "*.bmp", "*.PNG", "*.JPG", "*.JPEG",
];

/// `zenity --file-filter` value for [`IMAGE_PATTERNS`].
pub fn zenity_filter(label: &str) -> String {
    format!("{label} | {}", IMAGE_PATTERNS.join(" "))
}

/// First line of a dialog's stdout as a path, if it is absolute.
pub fn parse_dialog_output(stdout: &str) -> Option<PathBuf> {
    let line = stdout.lines().next()?.trim();
    let path = PathBuf::from(line);
    path.is_absolute().then_some(path)
}

/// Shows an image picker titled `title`. `Ok(None)` when the user cancels.
///
/// # Errors
///
/// Returns [`ShellError`] when no portal and no dialog tool is available.
pub fn pick_image(title: &str) -> Result<Option<PathBuf>, ShellError> {
    match pick_via_portal(title, PickKind::Image, false) {
        Ok(picked) => Ok(picked.into_iter().next()),
        Err(_) => pick_via_tools(title),
    }
}

/// Image picker that allows several files. Empty when the user cancels.
///
/// # Errors
///
/// Returns [`ShellError`] when no portal and no dialog tool is available.
pub fn pick_images(title: &str) -> Result<Vec<PathBuf>, ShellError> {
    match pick_via_portal(title, PickKind::Image, true) {
        Ok(picked) => Ok(picked),
        Err(_) => pick_images_via_tools(title),
    }
}

/// Absolute paths from zenity (`|` or newlines) or kdialog (`--separate-output`).
pub fn parse_dialog_paths(stdout: &str) -> Vec<PathBuf> {
    stdout
        .split(['\n', '|'])
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .collect()
}

/// Shows a file picker titled `title` (any type). `Ok(None)` when the user cancels.
///
/// # Errors
///
/// Returns [`ShellError`] when no portal and no dialog tool is available.
pub fn pick_file(title: &str) -> Result<Option<PathBuf>, ShellError> {
    match pick_via_portal(title, PickKind::File, false) {
        Ok(picked) => Ok(picked.into_iter().next()),
        Err(_) => pick_file_via_tools(title),
    }
}

/// Shows a folder picker titled `title`. `Ok(None)` when the user cancels.
///
/// # Errors
///
/// Returns [`ShellError`] when no portal and no dialog tool is available.
pub fn pick_folder(title: &str) -> Result<Option<PathBuf>, ShellError> {
    match pick_via_portal(title, PickKind::Folder, false) {
        Ok(picked) => Ok(picked.into_iter().next()),
        Err(_) => pick_folder_via_tools(title),
    }
}

enum PickKind {
    Image,
    File,
    Folder,
}

fn pick_folder_via_tools(title: &str) -> Result<Option<PathBuf>, ShellError> {
    if let Ok(out) = Command::new("zenity")
        .args(["--file-selection", "--directory", "--title", title])
        .output()
    {
        return Ok(parse_dialog_output(&String::from_utf8_lossy(&out.stdout)));
    }
    if let Ok(out) = Command::new("kdialog")
        .args(["--title", title, "--getexistingdirectory", "."])
        .output()
    {
        return Ok(parse_dialog_output(&String::from_utf8_lossy(&out.stdout)));
    }
    Err(ShellError::Bus(
        "no portal, zenity or kdialog for a folder picker".into(),
    ))
}

/// `directory`: pick a folder (portal option `directory`, version 3+)
/// instead of an image.
fn pick_via_portal(
    title: &str,
    kind: PickKind,
    multiple: bool,
) -> Result<Vec<PathBuf>, ShellError> {
    let connection = session()?;
    let unique = connection
        .unique_name()
        .ok_or_else(|| ShellError::Bus("session bus has no unique name".into()))?
        .as_str()
        .to_owned();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let token = format!("zalo_pick_{}_{nanos}", std::process::id());
    let request_path = portal_request_path(&unique, &token);
    let proxy = zbus::blocking::Proxy::new(
        &connection,
        PORTAL_SERVICE,
        request_path.as_str(),
        PORTAL_REQUEST_INTERFACE,
    )
    .map_err(|e| ShellError::Bus(e.to_string()))?;
    let mut signals = proxy
        .receive_signal("Response")
        .map_err(|e| ShellError::Bus(e.to_string()))?;

    let mut options: HashMap<&str, Value<'_>> = HashMap::new();
    options.insert("handle_token", Value::from(token.as_str()));
    options.insert("multiple", Value::from(multiple));
    match kind {
        PickKind::Folder => {
            options.insert("directory", Value::from(true));
        }
        PickKind::Image => {
            let patterns: Vec<(u32, String)> = IMAGE_PATTERNS
                .iter()
                .map(|p| (0_u32, (*p).to_owned()))
                .collect();
            options.insert("filters", Value::from(vec![("Ảnh".to_owned(), patterns)]));
        }
        PickKind::File => {}
    }
    connection
        .call_method(
            Some(PORTAL_SERVICE),
            PORTAL_PATH,
            Some(PORTAL_FILECHOOSER_INTERFACE),
            "OpenFile",
            &("", title, options),
        )
        .map_err(|e| ShellError::Bus(e.to_string()))?;
    let message = signals
        .next()
        .ok_or_else(|| ShellError::Bus("portal Response stream closed".into()))?;
    let (status, results): (u32, HashMap<String, OwnedValue>) = message
        .body()
        .deserialize()
        .map_err(|e| ShellError::Bus(e.to_string()))?;
    // 1 = cancelled by the user, 2 = ended another way.
    if status == 1 {
        return Ok(Vec::new());
    }
    if status != 0 {
        return Err(ShellError::Bus(format!("file chooser status {status}")));
    }
    let Some(uris) = results.get("uris") else {
        return Ok(Vec::new());
    };
    let uris: Vec<String> = uris
        .try_clone()
        .ok()
        .and_then(|v| Vec::<String>::try_from(v).ok())
        .unwrap_or_default();
    Ok(uris.iter().filter_map(|uri| parse_file_uri(uri)).collect())
}

fn pick_file_via_tools(title: &str) -> Result<Option<PathBuf>, ShellError> {
    if let Ok(out) = Command::new("zenity")
        .args(["--file-selection", "--title", title])
        .output()
    {
        return Ok(parse_dialog_output(&String::from_utf8_lossy(&out.stdout)));
    }
    if let Ok(out) = Command::new("kdialog")
        .args(["--title", title, "--getopenfilename", ".", "*"])
        .output()
    {
        return Ok(parse_dialog_output(&String::from_utf8_lossy(&out.stdout)));
    }
    Err(ShellError::Bus(
        "không có XDG FileChooser portal, zenity hay kdialog".into(),
    ))
}

fn pick_images_via_tools(title: &str) -> Result<Vec<PathBuf>, ShellError> {
    let filter = zenity_filter("Ảnh");
    let zenity = Command::new("zenity")
        .args([
            "--file-selection",
            "--multiple",
            "--separator",
            "|",
            "--title",
            title,
            "--file-filter",
            &filter,
        ])
        .output();
    if let Ok(out) = zenity {
        return Ok(parse_dialog_paths(&String::from_utf8_lossy(&out.stdout)));
    }
    let kfilter = format!("{}|Ảnh", IMAGE_PATTERNS.join(" "));
    let kdialog = Command::new("kdialog")
        .args([
            "--title",
            title,
            "--getopenfilename",
            ".",
            &kfilter,
            "--multiple",
            "--separate-output",
        ])
        .output();
    if let Ok(out) = kdialog {
        return Ok(parse_dialog_paths(&String::from_utf8_lossy(&out.stdout)));
    }
    Err(ShellError::Bus(
        "không có XDG FileChooser portal, zenity hay kdialog".into(),
    ))
}

fn pick_via_tools(title: &str) -> Result<Option<PathBuf>, ShellError> {
    let zenity = Command::new("zenity")
        .args([
            "--file-selection",
            "--title",
            title,
            "--file-filter",
            &zenity_filter("Ảnh"),
        ])
        .output();
    if let Ok(out) = zenity {
        return Ok(parse_dialog_output(&String::from_utf8_lossy(&out.stdout)));
    }
    let kdialog = Command::new("kdialog")
        .args([
            "--title",
            title,
            "--getopenfilename",
            ".",
            &format!("{}|Ảnh", IMAGE_PATTERNS.join(" ")),
        ])
        .output();
    if let Ok(out) = kdialog {
        return Ok(parse_dialog_output(&String::from_utf8_lossy(&out.stdout)));
    }
    Err(ShellError::Bus(
        "không có XDG FileChooser portal, zenity hay kdialog".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialog_output_and_filter() {
        assert_eq!(
            parse_dialog_output("/home/a/Ảnh 1.png\n"),
            Some(PathBuf::from("/home/a/Ảnh 1.png"))
        );
        assert_eq!(parse_dialog_output(""), None);
        assert_eq!(parse_dialog_output("relative.png"), None);
        assert!(zenity_filter("Ảnh").starts_with("Ảnh | *.png *.jpg"));
        assert_eq!(
            parse_dialog_paths("/tmp/a.png|/tmp/b.jpg\n/tmp/c.webp\nrelative.png\n"),
            vec![
                PathBuf::from("/tmp/a.png"),
                PathBuf::from("/tmp/b.jpg"),
                PathBuf::from("/tmp/c.webp"),
            ]
        );
    }
}
