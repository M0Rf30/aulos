// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The music folders the page can read ("roots"), picking them, their
//! permissions, and resolving virtual paths to browser `File`s.
//!
//! A root comes from one of two APIs:
//!
//! * `showDirectoryPicker` (Chromium) gives a `FileSystemDirectoryHandle`,
//!   which survives in IndexedDB; after a reload its permission is `prompt`
//!   until [`request_access`] runs inside a user gesture.
//! * `<input type=file webkitdirectory>` (Firefox, Safari) gives `File`
//!   objects, valid for this page session only: after a reload the root is
//!   known only by name (from the settings) and must be picked again.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use js_sys::{Array, Object, Reflect};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    File, FileSystemDirectoryHandle, FileSystemFileHandle, FileSystemHandle, FileSystemHandleKind,
};

use super::idb::{self, describe, error_name};
use crate::library::LibraryScanner;
use crate::virtual_path;

const ROOTS_KEY: &str = "roots";

/// Whether a root can be read now, and if not, what the user must do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootStatus {
    /// Files can be read.
    Ready,
    /// The browser remembers the folder but asks again before reading it:
    /// [`request_access`] from a click.
    NeedsPermission,
    /// The folder is known by name only (this browser cannot keep access
    /// across a reload): pick it again.
    NeedsRepick,
}

enum Backing {
    /// A folder handle and whether the page may read it.
    Handle {
        handle: FileSystemDirectoryHandle,
        granted: bool,
    },
    /// The files of a folder picked through `<input>`, by path relative to
    /// the root (`Artist/Album/01.flac`).
    Files(HashMap<String, File>),
}

struct Root {
    name: String,
    backing: Backing,
}

thread_local! {
    static ROOTS: RefCell<Vec<Root>> = const { RefCell::new(Vec::new()) };
}

fn with_root<T>(name: &str, f: impl FnOnce(&mut Root) -> T) -> Option<T> {
    ROOTS.with(|roots| {
        roots
            .borrow_mut()
            .iter_mut()
            .find(|r| r.name == name)
            .map(f)
    })
}

fn not_granted(root: &str) -> String {
    format!("Folder access for “{root}” was not granted — allow it in Settings › Library")
}

fn not_available(root: &str) -> String {
    format!("The files of “{root}” are not available — pick the folder again in Settings › Library")
}

/// What the page can do with the root of `path` (any path under it, or the
/// root itself); unknown roots need picking again.
#[must_use]
pub fn status(path: &Path) -> RootStatus {
    let Some((name, _)) = virtual_path::split(path) else {
        return RootStatus::NeedsRepick;
    };
    with_root(&name, |root| match &root.backing {
        Backing::Handle { granted: true, .. } | Backing::Files(_) => RootStatus::Ready,
        Backing::Handle { .. } => RootStatus::NeedsPermission,
    })
    .unwrap_or(RootStatus::NeedsRepick)
}

/// Whether this browser can keep folder access across reloads (a folder
/// picker that hands out handles).
#[must_use]
pub fn keeps_access_across_reloads() -> bool {
    picker_function().is_some()
}

fn picker_function() -> Option<js_sys::Function> {
    let window = web_sys::window()?;
    Reflect::get(&window, &JsValue::from_str("showDirectoryPicker"))
        .ok()?
        .dyn_into()
        .ok()
}

/// `handle.queryPermission({mode: "read"})`, or `requestPermission` when
/// `ask` (which prompts, so needs a user gesture). Called by name: the typed
/// bindings are behind web-sys's unstable-API cfg.
async fn permission_granted(handle: &FileSystemDirectoryHandle, ask: bool) -> bool {
    let method = if ask {
        "requestPermission"
    } else {
        "queryPermission"
    };
    let call = || -> Result<js_sys::Promise, JsValue> {
        let descriptor = Object::new();
        Reflect::set(&descriptor, &"mode".into(), &"read".into())?;
        let function: js_sys::Function = Reflect::get(handle, &method.into())?.dyn_into()?;
        function.call1(handle, &descriptor)?.dyn_into()
    };
    match call() {
        Ok(promise) => JsFuture::from(promise)
            .await
            .is_ok_and(|state| state.as_string().as_deref() == Some("granted")),
        Err(_) => false,
    }
}

async fn persist_roots() {
    let entries = Array::new();
    ROOTS.with(|roots| {
        for root in roots.borrow().iter() {
            if let Backing::Handle { handle, .. } = &root.backing {
                let entry = Object::new();
                let _ = Reflect::set(&entry, &"name".into(), &root.name.as_str().into());
                let _ = Reflect::set(&entry, &"handle".into(), handle);
                entries.push(&entry);
            }
        }
    });
    if let Err(error) = idb::put(ROOTS_KEY, &entries).await {
        tracing::warn!("The music folders could not be saved: {error}");
    }
}

/// Loads the folders remembered by earlier visits and asks the browser
/// whether each may still be read (without prompting).
pub(super) async fn restore() {
    let stored = match idb::get(ROOTS_KEY).await {
        Ok(Some(stored)) => stored,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!("The saved music folders could not be read: {error}");
            return;
        }
    };
    for entry in Array::from(&stored).iter() {
        let name = Reflect::get(&entry, &"name".into())
            .ok()
            .and_then(|n| n.as_string());
        let handle = Reflect::get(&entry, &"handle".into())
            .ok()
            .and_then(|h| h.dyn_into::<FileSystemDirectoryHandle>().ok());
        let (Some(name), Some(handle)) = (name, handle) else {
            continue;
        };
        let granted = permission_granted(&handle, false).await;
        ROOTS.with(|roots| {
            roots.borrow_mut().push(Root {
                name,
                backing: Backing::Handle { handle, granted },
            });
        });
    }
}

/// Asks the browser for read access to the folder of `path`. Must run inside
/// a user gesture (a click) to be allowed to prompt.
///
/// # Errors
/// A message when the folder is not a known handle-backed root.
pub async fn request_access(path: &Path) -> Result<RootStatus, String> {
    let (name, _) = virtual_path::split(path).ok_or("not a library folder")?;
    let handle = with_root(&name, |root| match &root.backing {
        Backing::Handle { handle, .. } => Some(handle.clone()),
        Backing::Files(_) => None,
    })
    .flatten()
    .ok_or_else(|| not_available(&name))?;
    let granted = permission_granted(&handle, true).await;
    with_root(&name, |root| {
        if let Backing::Handle { granted: g, .. } = &mut root.backing {
            *g = granted;
        }
    });
    Ok(status(path))
}

/// Forgets the folder `path` (a root): its handle, its in-memory files and
/// its saved entry.
pub async fn forget_root(path: &Path) {
    let Some((name, _)) = virtual_path::split(path) else {
        return;
    };
    let removed = ROOTS.with(|roots| {
        let mut roots = roots.borrow_mut();
        let before = roots.len();
        roots.retain(|r| r.name != name);
        roots.len() != before
    });
    if removed {
        persist_roots().await;
    }
}

/// Lets the user pick a music folder and returns its root's virtual path.
/// `None` when they cancel. Must run inside a user gesture.
///
/// # Errors
/// A message when the browser refused or the folder has no files.
pub async fn pick_folder() -> Result<Option<PathBuf>, String> {
    match picker_function() {
        Some(picker) => pick_handle(&picker).await,
        None => pick_with_input().await,
    }
}

async fn pick_handle(picker: &js_sys::Function) -> Result<Option<PathBuf>, String> {
    let window = web_sys::window().ok_or("no window")?;
    let options = Object::new();
    Reflect::set(&options, &"mode".into(), &"read".into()).map_err(|e| describe(&e))?;
    let promise: js_sys::Promise = picker
        .call1(&window, &options)
        .map_err(|e| describe(&e))?
        .dyn_into()
        .map_err(|_| "the folder picker did not answer".to_owned())?;
    let handle: FileSystemDirectoryHandle = match JsFuture::from(promise).await {
        Ok(handle) => handle
            .dyn_into()
            .map_err(|_| "the folder picker returned no folder".to_owned())?,
        Err(error) if error_name(&error) == "AbortError" => return Ok(None),
        Err(error) => return Err(describe(&error)),
    };

    // The same folder picked again keeps its root (and so its tracks).
    let existing: Vec<(String, FileSystemDirectoryHandle)> = ROOTS.with(|roots| {
        roots
            .borrow()
            .iter()
            .filter_map(|r| match &r.backing {
                Backing::Handle { handle, .. } => Some((r.name.clone(), handle.clone())),
                Backing::Files(_) => None,
            })
            .collect()
    });
    let mut same = None;
    for (name, other) in existing {
        let this: &FileSystemHandle = handle.as_ref();
        let other: &FileSystemHandle = other.as_ref();
        if JsFuture::from(this.is_same_entry(other))
            .await
            .is_ok_and(|same| same.as_bool() == Some(true))
        {
            same = Some(name);
            break;
        }
    }
    let name = same.unwrap_or_else(|| {
        let wanted = virtual_path::root_name(&handle.name());
        ROOTS.with(|roots| {
            let roots = roots.borrow();
            virtual_path::unique_root_name(&wanted, |n| roots.iter().any(|r| r.name == n))
        })
    });
    ROOTS.with(|roots| {
        let mut roots = roots.borrow_mut();
        let backing = Backing::Handle {
            handle,
            granted: true,
        };
        match roots.iter_mut().find(|r| r.name == name) {
            Some(root) => root.backing = backing,
            None => roots.push(Root {
                name: name.clone(),
                backing,
            }),
        }
    });
    persist_roots().await;
    Ok(Some(virtual_path::root_path(&name)))
}

/// The `webkitRelativePath` of a picked file.
fn relative_path(file: &File) -> String {
    Reflect::get(file, &"webkitRelativePath".into())
        .ok()
        .and_then(|p| p.as_string())
        .unwrap_or_default()
}

async fn pick_with_input() -> Result<Option<PathBuf>, String> {
    let files = choose_files().await?;
    let Some(files) = files else {
        return Ok(None);
    };
    let mut tree = HashMap::new();
    let mut folder = None;
    for file in files {
        let path = relative_path(&file);
        let mut parts = path.splitn(2, '/');
        let (top, below) = (parts.next().unwrap_or_default(), parts.next());
        let Some(below) = below else {
            continue;
        };
        folder.get_or_insert_with(|| top.to_owned());
        tree.insert(below.to_owned(), file);
    }
    let Some(folder) = folder else {
        return Err("The folder has no files".to_owned());
    };
    let wanted = virtual_path::root_name(&folder);
    // Picking a folder of the same name again re-attaches its files.
    let name = ROOTS.with(|roots| {
        let roots = roots.borrow();
        match roots.iter().find(|r| r.name == wanted) {
            Some(Root {
                backing: Backing::Handle { .. },
                ..
            }) => virtual_path::unique_root_name(&wanted, |n| roots.iter().any(|r| r.name == n)),
            _ => wanted.clone(),
        }
    });
    ROOTS.with(|roots| {
        let mut roots = roots.borrow_mut();
        match roots.iter_mut().find(|r| r.name == name) {
            Some(root) => root.backing = Backing::Files(tree),
            None => roots.push(Root {
                name: name.clone(),
                backing: Backing::Files(tree),
            }),
        }
    });
    Ok(Some(virtual_path::root_path(&name)))
}

/// Opens the `<input type=file webkitdirectory>` dialog; `None` on cancel.
async fn choose_files() -> Result<Option<Vec<File>>, String> {
    let document = web_sys::window()
        .and_then(|w| w.document())
        .ok_or("no document")?;
    let input: web_sys::HtmlInputElement = document
        .create_element("input")
        .map_err(|e| describe(&e))?
        .dyn_into()
        .map_err(|_| "no input element".to_owned())?;
    input.set_type("file");
    input.set_multiple(true);
    input
        .set_attribute("webkitdirectory", "")
        .map_err(|e| describe(&e))?;
    input
        .set_attribute("style", "display:none")
        .map_err(|e| describe(&e))?;
    let body = document.body().ok_or("no document body")?;
    body.append_child(&input).map_err(|e| describe(&e))?;

    let answer = js_sys::Promise::new(&mut |resolve, _| {
        let picked = {
            let input = input.clone();
            let resolve = resolve.clone();
            Closure::once_into_js(move || {
                let files = input.files().map_or_else(Array::new, |list| {
                    (0..list.length()).filter_map(|i| list.item(i)).collect()
                });
                let _ = resolve.call1(&JsValue::NULL, &files);
            })
        };
        let cancelled = Closure::once_into_js(move || {
            let _ = resolve.call1(&JsValue::NULL, &JsValue::NULL);
        });
        let _ = input.add_event_listener_with_callback("change", picked.unchecked_ref());
        let _ = input.add_event_listener_with_callback("cancel", cancelled.unchecked_ref());
    });
    input.click();
    let result = JsFuture::from(answer).await.map_err(|e| describe(&e));
    input.remove();
    let picked = result?;
    if picked.is_null() {
        return Ok(None);
    }
    let files: Vec<File> = Array::from(&picked)
        .iter()
        .filter_map(|f| f.dyn_into().ok())
        .collect();
    Ok((!files.is_empty()).then_some(files))
}

/// Where a virtual path points, before the file itself is fetched.
enum Located {
    File(File),
    Handle {
        root: String,
        dir: FileSystemDirectoryHandle,
        parts: Vec<String>,
    },
}

fn locate(path: &Path) -> Result<Located, String> {
    let (name, parts) = virtual_path::split(path)
        .filter(|(_, parts)| !parts.is_empty())
        .ok_or_else(|| format!("{} is not a file of a music folder", path.display()))?;
    let located = with_root(&name, |root| match &root.backing {
        Backing::Files(files) => files
            .get(&parts.join("/"))
            .cloned()
            .map(Located::File)
            .ok_or_else(|| format!("“{}” is not in “{name}” any more", parts.join("/"))),
        Backing::Handle { granted: false, .. } => Err(not_granted(&name)),
        Backing::Handle { handle, .. } => Ok(Located::Handle {
            root: name.clone(),
            dir: handle.clone(),
            parts: parts.clone(),
        }),
    });
    located.unwrap_or_else(|| Err(not_available(&name)))
}

/// A failed handle call as a user-readable message; permission errors also
/// mark the root as needing permission again.
fn handle_error(root: &str, what: &str, error: &JsValue) -> String {
    match error_name(error).as_str() {
        "NotAllowedError" | "SecurityError" => {
            with_root(root, |r| {
                if let Backing::Handle { granted, .. } = &mut r.backing {
                    *granted = false;
                }
            });
            not_granted(root)
        }
        "NotFoundError" | "TypeMismatchError" => format!("“{what}” is not in “{root}” any more"),
        _ => format!("“{what}” could not be read: {}", describe(error)),
    }
}

/// The browser `File` at the virtual path `path`.
///
/// # Errors
/// A user-readable message: the root is unknown or lacks permission, or the
/// file is gone.
pub async fn open_file(path: &Path) -> Result<File, String> {
    match locate(path)? {
        Located::File(file) => Ok(file),
        Located::Handle { root, dir, parts } => {
            let what = parts.join("/");
            let (last, folders) = parts.split_last().ok_or("empty path")?;
            let mut dir = dir;
            for folder in folders {
                dir = JsFuture::from(dir.get_directory_handle(folder))
                    .await
                    .map_err(|e| handle_error(&root, &what, &e))?
                    .unchecked_into();
            }
            let handle: FileSystemFileHandle = JsFuture::from(dir.get_file_handle(last))
                .await
                .map_err(|e| handle_error(&root, &what, &e))?
                .unchecked_into();
            JsFuture::from(handle.get_file())
                .await
                .map(JsCast::unchecked_into)
                .map_err(|e| handle_error(&root, &what, &e))
        }
    }
}

/// Size in bytes and last-modified Unix seconds of a `File`.
#[must_use]
pub fn info_of(file: &File) -> (u64, i64) {
    let size = web_sys::Blob::size(file) as u64;
    (size, (file.last_modified() / 1000.0) as i64)
}

/// Bytes `[start, start + len)` of `file` (fewer at the end).
///
/// # Errors
/// A message when the browser cannot read the file (it vanished or changed).
pub async fn read_range(file: &File, start: u64, len: u64) -> Result<Vec<u8>, String> {
    let blob = file
        .slice_with_f64_and_f64(start as f64, (start + len) as f64)
        .map_err(|e| describe(&e))?;
    let buffer = JsFuture::from(blob.array_buffer())
        .await
        .map_err(|e| format!("the file could not be read: {}", describe(&e)))?;
    Ok(js_sys::Uint8Array::new(&buffer).to_vec())
}

/// An audio file found by [`list_audio`], with the `File` to read it from.
pub struct ListedFile {
    /// Its virtual path.
    pub path: PathBuf,
    /// The browser file.
    pub file: File,
}

impl ListedFile {
    /// Size in bytes and last-modified Unix seconds.
    #[must_use]
    pub fn info(&self) -> (u64, i64) {
        info_of(&self.file)
    }
}

/// The audio files under the root `root`, and whether the walk saw every
/// folder (so that a file missing from it is truly gone).
pub struct Listing {
    pub files: Vec<ListedFile>,
    pub complete: bool,
}

/// Walks the root `root` for audio files.
///
/// # Errors
/// A message when the root cannot be read at all (unknown, no permission).
pub async fn list_audio(root: &Path) -> Result<Listing, String> {
    let (name, _) = virtual_path::split(root).ok_or("not a music folder")?;
    enum Source {
        Files(Vec<(String, File)>),
        Handle(FileSystemDirectoryHandle),
    }
    let source = with_root(&name, |r| match &r.backing {
        Backing::Files(files) => Ok(Source::Files(
            files.iter().map(|(p, f)| (p.clone(), f.clone())).collect(),
        )),
        Backing::Handle { granted: false, .. } => Err(not_granted(&name)),
        Backing::Handle { handle, .. } => Ok(Source::Handle(handle.clone())),
    })
    .unwrap_or_else(|| Err(not_available(&name)))?;

    match source {
        Source::Files(files) => Ok(Listing {
            files: files
                .into_iter()
                .filter(|(rel, _)| LibraryScanner::is_audio_file(Path::new(rel)))
                .map(|(rel, file)| ListedFile {
                    path: virtual_path::join(&name, &rel),
                    file,
                })
                .collect(),
            complete: true,
        }),
        Source::Handle(top) => {
            let mut listing = Listing {
                files: Vec::new(),
                complete: true,
            };
            let mut pending = vec![(top, String::new())];
            while let Some((dir, prefix)) = pending.pop() {
                if let Err(error) = walk_dir(&name, &dir, &prefix, &mut pending, &mut listing).await
                {
                    if prefix.is_empty() {
                        return Err(error);
                    }
                    tracing::warn!("Skipping “{prefix}” in “{name}”: {error}");
                    listing.complete = false;
                }
            }
            Ok(listing)
        }
    }
}

/// The entries of `dir`: audio files go to `listing`, folders to `pending`.
async fn walk_dir(
    root: &str,
    dir: &FileSystemDirectoryHandle,
    prefix: &str,
    pending: &mut Vec<(FileSystemDirectoryHandle, String)>,
    listing: &mut Listing,
) -> Result<(), String> {
    let entries = dir.values();
    loop {
        let step = JsFuture::from(entries.next().map_err(|e| describe(&e))?)
            .await
            .map_err(|e| handle_error(root, prefix, &e))?;
        if Reflect::get(&step, &"done".into())
            .ok()
            .and_then(|d| d.as_bool())
            == Some(true)
        {
            return Ok(());
        }
        let Ok(entry) =
            Reflect::get(&step, &"value".into()).and_then(|v| v.dyn_into::<FileSystemHandle>())
        else {
            continue;
        };
        let rel = if prefix.is_empty() {
            entry.name()
        } else {
            format!("{prefix}/{}", entry.name())
        };
        match entry.kind() {
            FileSystemHandleKind::Directory => {
                pending.push((entry.unchecked_into(), rel));
            }
            _ if LibraryScanner::is_audio_file(Path::new(&rel)) => {
                let handle: FileSystemFileHandle = entry.unchecked_into();
                match JsFuture::from(handle.get_file()).await {
                    Ok(file) => listing.files.push(ListedFile {
                        path: virtual_path::join(root, &rel),
                        file: file.unchecked_into(),
                    }),
                    Err(error) => {
                        tracing::warn!("Skipping “{rel}”: {}", describe(&error));
                        listing.complete = false;
                    }
                }
            }
            _ => {}
        }
    }
}

/// The names of the files directly inside the virtual folder `dir`.
///
/// # Errors
/// A message when the folder cannot be read.
pub async fn list_dir(dir: &Path) -> Result<Vec<String>, String> {
    let (name, parts) = virtual_path::split(dir).ok_or("not a music folder")?;
    enum Source {
        Names(Vec<String>),
        Handle(FileSystemDirectoryHandle),
    }
    let source = with_root(&name, |r| match &r.backing {
        Backing::Files(files) => {
            let prefix = if parts.is_empty() {
                String::new()
            } else {
                format!("{}/", parts.join("/"))
            };
            Ok(Source::Names(
                files
                    .keys()
                    .filter_map(|rel| rel.strip_prefix(&prefix))
                    .filter(|rest| !rest.contains('/'))
                    .map(str::to_owned)
                    .collect(),
            ))
        }
        Backing::Handle { granted: false, .. } => Err(not_granted(&name)),
        Backing::Handle { handle, .. } => Ok(Source::Handle(handle.clone())),
    })
    .unwrap_or_else(|| Err(not_available(&name)))?;
    let mut handle = match source {
        Source::Handle(handle) => handle,
        Source::Names(names) => return Ok(names),
    };
    for part in &parts {
        handle = JsFuture::from(handle.get_directory_handle(part))
            .await
            .map_err(|e| handle_error(&name, part, &e))?
            .unchecked_into();
    }
    let mut names = Vec::new();
    let entries = handle.keys();
    loop {
        let step = JsFuture::from(entries.next().map_err(|e| describe(&e))?)
            .await
            .map_err(|e| handle_error(&name, "", &e))?;
        if Reflect::get(&step, &"done".into())
            .ok()
            .and_then(|d| d.as_bool())
            == Some(true)
        {
            return Ok(names);
        }
        if let Some(entry) = Reflect::get(&step, &"value".into())
            .ok()
            .and_then(|v| v.as_string())
        {
            names.push(entry);
        }
    }
}
