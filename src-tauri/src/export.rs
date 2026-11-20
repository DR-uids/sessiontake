use crate::catalog::{Failure, Roots};
use crate::{
    current_preview,
    preview::{CodeBlock, Format},
    CurrentSession,
};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

#[cfg(target_os = "macos")]
mod native {
    use crate::catalog::Failure;
    use std::{
        ffi::{c_char, CStr, CString},
        os::unix::ffi::OsStrExt,
        path::PathBuf,
    };

    extern "C" {
        fn session_take_save_path(filename: *const c_char, label: *const c_char) -> *mut c_char;
        fn session_take_free_path(path: *mut c_char);
        fn session_take_copy(bytes: *const u8, length: usize) -> bool;
    }

    // AppKit calls are dispatched to the main thread by the Tauri commands.
    pub fn save_path(filename: String, label: String) -> Result<Option<PathBuf>, Failure> {
        let filename = CString::new(filename).map_err(|_| Failure::SaveDialog)?;
        let label = CString::new(label).map_err(|_| Failure::SaveDialog)?;
        let raw = unsafe { session_take_save_path(filename.as_ptr(), label.as_ptr()) };
        if raw.is_null() {
            return Ok(None);
        }
        let path = PathBuf::from(std::ffi::OsStr::from_bytes(
            unsafe { CStr::from_ptr(raw) }.to_bytes(),
        ));
        unsafe { session_take_free_path(raw) };
        Ok(Some(path))
    }

    pub fn copy(body: &str) -> Result<(), Failure> {
        if unsafe { session_take_copy(body.as_ptr(), body.len()) } {
            Ok(())
        } else {
            Err(Failure::Clipboard)
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod native {
    use crate::catalog::Failure;
    use std::path::PathBuf;

    pub fn save_path(_: String, _: String) -> Result<Option<PathBuf>, Failure> {
        Err(Failure::SaveDialog)
    }

    pub fn copy(_: &str) -> Result<(), Failure> {
        Err(Failure::Clipboard)
    }
}

#[tauri::command]
pub async fn copy_source(
    window: tauri::Window,
    current: tauri::State<'_, CurrentSession>,
    request: u64,
    format: Format,
) -> Result<(), Failure> {
    let preview = current_preview(&current, request)?;
    let preview = tauri::async_runtime::spawn_blocking(move || {
        preview.source(format);
        preview
    })
    .await
    .map_err(|_| Failure::Io)?;
    copy_on_main_thread(window, move || native::copy(preview.source(format))).await
}

#[tauri::command]
pub async fn copy_block(
    window: tauri::Window,
    current: tauri::State<'_, CurrentSession>,
    request: u64,
    target: CodeBlock,
) -> Result<(), Failure> {
    let preview = current_preview(&current, request)?;
    let body =
        tauri::async_runtime::spawn_blocking(move || preview.code_block(target).map(str::to_owned))
            .await
            .map_err(|_| Failure::Io)??;
    copy_on_main_thread(window, move || native::copy(&body)).await
}

async fn copy_on_main_thread(
    window: tauri::Window,
    copy: impl FnOnce() -> Result<(), Failure> + Send + 'static,
) -> Result<(), Failure> {
    let (send, receive) = tokio::sync::oneshot::channel();
    window
        .run_on_main_thread(move || {
            let _ = send.send(copy());
        })
        .map_err(|_| Failure::Clipboard)?;
    receive.await.map_err(|_| Failure::Clipboard)?
}

#[tauri::command]
pub async fn save_export(
    window: tauri::Window,
    current: tauri::State<'_, CurrentSession>,
    request: u64,
    format: Format,
    filename: String,
    label: String,
) -> Result<Option<String>, Failure> {
    let preview = current_preview(&current, request)?;
    let (send, receive) = tokio::sync::oneshot::channel();
    window
        .run_on_main_thread(move || {
            let _ = send.send(native::save_path(filename, label));
        })
        .map_err(|_| Failure::SaveDialog)?;
    let Some(path) = receive.await.map_err(|_| Failure::SaveDialog)?? else {
        return Ok(None);
    };
    tauri::async_runtime::spawn_blocking(move || {
        write_export(&path, preview.source(format).as_bytes())
    })
    .await
    .map_err(|_| Failure::Io)?
    .map(Some)
}

fn write_export(path: &Path, bytes: &[u8]) -> Result<String, Failure> {
    write_to(path, bytes, &Roots::local()?.all())
}

fn write_to(path: &Path, bytes: &[u8], roots: &[&Path]) -> Result<String, Failure> {
    let name = path.file_name().ok_or(Failure::SaveDialog)?;
    let parent = fs::canonicalize(path.parent().ok_or(Failure::SaveDialog)?)?;
    let path = parent.join(name);
    let resolved = canonical_if_exists(&path)?;
    for root in roots {
        if path.starts_with(root)
            || resolved
                .as_deref()
                .is_some_and(|path| path.starts_with(root))
        {
            return Err(Failure::SourceDestination);
        }
        if let Some(root) = canonical_if_exists(root).ok().flatten() {
            if path.starts_with(&root)
                || resolved
                    .as_deref()
                    .is_some_and(|path| path.starts_with(&root))
            {
                return Err(Failure::SourceDestination);
            }
        }
    }
    // Replace directory entries atomically. Existing files, including any
    // hard-linked alias of a source record, are never opened for writing.
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path)
        .map_err(|error| Failure::from(error.error))?;
    Ok(name.to_string_lossy().into_owned())
}

fn canonical_if_exists(path: &Path) -> Result<Option<PathBuf>, Failure> {
    match fs::canonicalize(path) {
        Ok(path) => Ok(Some(path)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_preserves_utf8_and_rejects_source_destinations() {
        let workspace = tempfile::tempdir().unwrap();
        let root = workspace.path().join("history");
        fs::create_dir(&root).unwrap();
        let source = root.join("original.jsonl");
        fs::write(&source, "source\n").unwrap();
        let body = "你好\nline 2\n";
        let export = workspace.path().join("2026-11-19-export.md");
        assert_eq!(
            write_to(&export, body.as_bytes(), &[&root]),
            Ok("2026-11-19-export.md".into())
        );
        assert_eq!(fs::read(&export).unwrap(), body.as_bytes());
        assert_eq!(
            write_to(&source, b"overwrite", &[&root]),
            Err(Failure::SourceDestination)
        );

        #[cfg(unix)]
        {
            let link = workspace.path().join("link.jsonl");
            std::os::unix::fs::symlink(&source, &link).unwrap();
            assert_eq!(
                write_to(&link, b"overwrite", &[&root]),
                Err(Failure::SourceDestination)
            );
            let alias = workspace.path().join("alias.jsonl");
            fs::hard_link(&source, &alias).unwrap();
            write_to(&alias, body.as_bytes(), &[&root]).unwrap();
        }
        assert_eq!(fs::read_to_string(&source).unwrap(), "source\n");
    }
}
