use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use serde::Deserialize;
use tauri::AppHandle;
use tauri_nspanel::{
    ManagerExt,
    objc2::{AnyThread, MainThreadMarker, rc::autoreleasepool},
    objc2_app_kit::{
        NSApplication, NSBitmapImageRep, NSModalResponseOK, NSPasteboard, NSPasteboardTypePNG,
        NSSavePanel, NSWorkspace,
    },
    objc2_foundation::{NSArray, NSData, NSString, NSURL},
};
use thiserror::Error;

const MAX_CSV_BYTES: usize = 32 * 1024 * 1024;
const MAX_PNG_BYTES: usize = 16 * 1024 * 1024;
const MAX_IMAGE_PIXELS: u64 = 16 * 1024 * 1024;
static SAVE_OPEN: AtomicBool = AtomicBool::new(false);
static TEMPORARY_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    Csv,
    Png,
}

impl ExportFormat {
    fn extension(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Png => "png",
        }
    }
}

#[derive(Debug, Error)]
pub enum ExportError {
    #[error("Finish the open save dialog before exporting again.")]
    Busy,
    #[error("The export is empty, too large or not a valid {0} file.")]
    InvalidData(&'static str),
    #[error("{0}")]
    Data(String),
    #[error("macOS could not open the save dialog. Try again.")]
    Dialog,
    #[error("Could not save the export: {0}")]
    Write(#[from] std::io::Error),
    #[error("macOS could not copy the chart. Try again.")]
    Clipboard,
    #[error("The history folder does not exist yet. Save a reading first.")]
    HistoryMissing,
    #[error("macOS could not open the history folder.")]
    Finder,
}

struct SaveGuard;

impl SaveGuard {
    fn acquire() -> Result<Self, ExportError> {
        SAVE_OPEN
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| ExportError::Busy)?;
        Ok(Self)
    }
}

impl Drop for SaveGuard {
    fn drop(&mut self) {
        SAVE_OPEN.store(false, Ordering::Release);
    }
}

pub(super) fn dialog_open() -> bool {
    SAVE_OPEN.load(Ordering::Acquire)
}

pub async fn save(
    app: &AppHandle,
    format: ExportFormat,
    bytes: Vec<u8>,
) -> Result<Option<String>, ExportError> {
    let bytes = tauri::async_runtime::spawn_blocking(move || {
        validate(format, &bytes)?;
        Ok::<_, ExportError>(bytes)
    })
    .await
    .map_err(|_| ExportError::Dialog)??;
    save_with(app, format, move || Ok(bytes)).await
}

pub async fn save_with(
    app: &AppHandle,
    format: ExportFormat,
    producer: impl FnOnce() -> Result<Vec<u8>, ExportError> + Send + 'static,
) -> Result<Option<String>, ExportError> {
    let guard = SaveGuard::acquire()?;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let handle = app.clone();
    app.run_on_main_thread(move || {
        // Keep the guard on the main thread until AppKit ends its modal event loop.
        let _guard = guard;
        let result = (|| {
            let panel = handle
                .get_webview_panel(super::WINDOW)
                .map_err(|_| ExportError::Dialog)?;
            let was_visible = panel.is_visible();
            let was_key = panel.as_panel().isKeyWindow();
            // AppKit can reset the save panel's level when its modal session starts.
            // Order out the floating panel without resetting its content or position.
            panel.hide();
            let result = choose_destination(format);
            if was_visible {
                if was_key {
                    panel.make_key_and_order_front();
                } else {
                    panel.show();
                }
            }
            result
        })();
        let _ = sender.send(result);
    })
    .map_err(|_| ExportError::Dialog)?;
    let Some(path) = receiver.await.map_err(|_| ExportError::Dialog)?? else {
        return Ok(None);
    };
    tauri::async_runtime::spawn_blocking(move || {
        let bytes = producer()?;
        validate(format, &bytes)?;
        atomic_write(&path, &bytes)?;
        Ok(Some(path.to_string_lossy().into_owned()))
    })
    .await
    .map_err(|_| ExportError::Dialog)?
}

pub async fn copy_png(app: &AppHandle, bytes: Vec<u8>) -> Result<(), ExportError> {
    let bytes = tauri::async_runtime::spawn_blocking(move || {
        validate(ExportFormat::Png, &bytes)?;
        Ok::<_, ExportError>(bytes)
    })
    .await
    .map_err(|_| ExportError::Clipboard)??;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let result = autoreleasepool(|_| {
            let clipboard = NSPasteboard::generalPasteboard();
            let data = NSData::with_bytes(&bytes);
            clipboard.clearContents();
            if clipboard.setData_forType(Some(&data), unsafe { NSPasteboardTypePNG }) {
                Ok(())
            } else {
                Err(ExportError::Clipboard)
            }
        });
        let _ = sender.send(result);
    })
    .map_err(|_| ExportError::Clipboard)?;
    receiver.await.map_err(|_| ExportError::Clipboard)?
}

pub async fn reveal_history_folder(app: &AppHandle) -> Result<(), ExportError> {
    let directory = std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".local/share/delta-v/history"))
        .filter(|path| path.is_dir())
        .ok_or(ExportError::HistoryMissing)?;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let result = autoreleasepool(|_| {
            let path = NSString::from_str(&directory.to_string_lossy());
            let url = NSURL::fileURLWithPath_isDirectory(&path, true);
            if NSWorkspace::sharedWorkspace().openURL(&url) {
                Ok(())
            } else {
                Err(ExportError::Finder)
            }
        });
        let _ = sender.send(result);
    })
    .map_err(|_| ExportError::Finder)?;
    receiver.await.map_err(|_| ExportError::Finder)?
}

fn validate(format: ExportFormat, bytes: &[u8]) -> Result<(), ExportError> {
    match format {
        ExportFormat::Csv => {
            if bytes.is_empty()
                || bytes.len() > MAX_CSV_BYTES
                || bytes.contains(&0)
                || std::str::from_utf8(bytes).is_err()
            {
                return Err(ExportError::InvalidData("CSV"));
            }
        }
        ExportFormat::Png => {
            if bytes.len() < 33
                || bytes.len() > MAX_PNG_BYTES
                || &bytes[..8] != b"\x89PNG\r\n\x1a\n"
                || &bytes[8..16] != b"\0\0\0\rIHDR"
            {
                return Err(ExportError::InvalidData("PNG"));
            }
            let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
            let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
            if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS
            {
                return Err(ExportError::InvalidData("PNG"));
            }
            autoreleasepool(|_| {
                let data = NSData::with_bytes(bytes);
                let image = NSBitmapImageRep::initWithData(NSBitmapImageRep::alloc(), &data)
                    .ok_or(ExportError::InvalidData("PNG"))?;
                // Header checks bound memory before AppKit decodes the compressed pixel data.
                if image.pixelsWide() != width as isize
                    || image.pixelsHigh() != height as isize
                    || image.bitmapData().is_null()
                {
                    return Err(ExportError::InvalidData("PNG"));
                }
                Ok(())
            })?;
        }
    }
    Ok(())
}

// These APIs support macOS 13 without adding a UniformTypeIdentifiers dependency.
#[allow(deprecated)]
fn choose_destination(format: ExportFormat) -> Result<Option<PathBuf>, ExportError> {
    let mtm = MainThreadMarker::new().ok_or(ExportError::Dialog)?;
    let panel = NSSavePanel::savePanel(mtm);
    let extension = NSString::from_str(format.extension());
    panel.setAllowedFileTypes(Some(&NSArray::from_slice(&[&*extension])));
    panel.setAllowsOtherFileTypes(false);
    panel.setCanCreateDirectories(true);
    panel.setExtensionHidden(false);
    panel.setNameFieldStringValue(&NSString::from_str(&format!(
        "Delta-V-history.{}",
        format.extension()
    )));
    panel.setTitle(Some(&NSString::from_str("Export usage history")));
    NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
    let response = panel.runModal();
    // Finish removing the modal window before handing focus back to Delta-V.
    panel.orderOut(None);
    if response != NSModalResponseOK {
        return Ok(None);
    }
    let url = panel.URL().ok_or(ExportError::Dialog)?;
    if !url.isFileURL() {
        return Err(ExportError::Dialog);
    }
    let path = url.path().ok_or(ExportError::Dialog)?.to_string();
    Ok(Some(PathBuf::from(path)))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), ExportError> {
    let parent = path.parent().ok_or(ExportError::Dialog)?;
    if fs::symlink_metadata(path).is_ok_and(|metadata| !metadata.is_file()) {
        return Err(std::io::Error::other("Choose a regular file for the export.").into());
    }
    let suffix = TEMPORARY_ID.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".delta-v-export-{}-{suffix}.tmp",
        std::process::id()
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    Ok(result?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_or_oversized_exports() {
        assert!(validate(ExportFormat::Csv, b"provider,used\r\nClaude,30\r\n").is_ok());
        assert!(validate(ExportFormat::Csv, b"").is_err());
        assert!(validate(ExportFormat::Csv, b"a\0b").is_err());
        assert!(validate(ExportFormat::Csv, &[0xff]).is_err());
        assert!(validate(ExportFormat::Csv, &vec![b'x'; MAX_CSV_BYTES + 1]).is_err());
        assert!(validate(ExportFormat::Png, b"not an image").is_err());
        let mut png = include_bytes!("../../icons/128x128.png").to_vec();
        assert!(validate(ExportFormat::Png, &png).is_ok());
        assert!(validate(ExportFormat::Png, &png[..33]).is_err());
        png[16..20].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(validate(ExportFormat::Png, &png).is_err());
    }

    #[test]
    fn writes_private_export_and_preserves_symlink_target() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let directory = std::env::temp_dir().join(format!(
            "delta-v-export-test-{}-{}",
            std::process::id(),
            TEMPORARY_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("history.csv");
        atomic_write(&path, b"first").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"first");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        atomic_write(&path, b"second").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
        let link = directory.join("link.csv");
        symlink(&path, &link).unwrap();
        assert!(atomic_write(&link, b"unexpected").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"second");
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 2);
        fs::remove_dir_all(directory).unwrap();
    }
}
