#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod catalog;
mod export;
mod fences;
mod preview;
mod search;
mod transcript;

use catalog::{Catalog, Failure, Indexed, Roots, Session, Source};
use preview::{Preview, View};
use serde::Serialize;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tauri::State;

type Index = Mutex<HashMap<Source, HashMap<String, Indexed>>>;

#[derive(Default)]
struct ActiveSession {
    request: u64,
    preview: Option<Arc<Preview>>,
}

type CurrentSession = Mutex<ActiveSession>;

fn current_preview(state: &CurrentSession, request: u64) -> Result<Arc<Preview>, Failure> {
    let active = state.lock().map_err(|_| Failure::Io)?;
    if active.request != request {
        return Err(Failure::Missing);
    }
    active.preview.clone().ok_or(Failure::Missing)
}

#[derive(Serialize)]
struct Listing {
    sessions: Vec<Session>,
    warning: Option<Failure>,
}

#[tauri::command]
async fn list_sessions(source: Source, state: State<'_, Index>) -> Result<Listing, Failure> {
    let catalog = tauri::async_runtime::spawn_blocking(move || {
        Roots::local().map(|roots| Catalog::load(&roots, source))
    })
    .await
    .map_err(|_| Failure::Index)??;
    let sessions = catalog
        .sessions
        .iter()
        .map(|item| item.session.clone())
        .collect();
    let index = catalog
        .sessions
        .into_iter()
        .map(|item| (item.session.key.clone(), item))
        .collect();
    state
        .lock()
        .map_err(|_| Failure::Index)?
        .insert(source, index);
    Ok(Listing {
        sessions,
        warning: catalog.warning,
    })
}

#[tauri::command]
async fn read_session(
    source: Source,
    key: String,
    request: u64,
    state: State<'_, Index>,
    current: State<'_, CurrentSession>,
) -> Result<usize, Failure> {
    {
        let mut active = current.lock().map_err(|_| Failure::Io)?;
        if request <= active.request {
            return Err(Failure::Missing);
        }
        active.request = request;
        active.preview = None;
    }
    let record = state
        .lock()
        .map_err(|_| Failure::Index)?
        .get(&source)
        .and_then(|index| index.get(&key))
        .cloned()
        .ok_or(Failure::Missing)?;
    let preview =
        tauri::async_runtime::spawn_blocking(move || transcript::read(record).map(Preview::new))
            .await
            .map_err(|_| Failure::Io)??;
    let mut active = current.lock().map_err(|_| Failure::Io)?;
    if active.request != request {
        return Err(Failure::Missing);
    }
    let count = preview.message_count;
    active.preview = Some(Arc::new(preview));
    Ok(count)
}

#[tauri::command]
async fn read_view(
    current: State<'_, CurrentSession>,
    request: u64,
    view: View,
    page: usize,
) -> Result<serde_json::Value, Failure> {
    let preview = current_preview(&current, request)?;
    tauri::async_runtime::spawn_blocking(move || {
        serde_json::to_value(preview.view(view, page)?).map_err(|_| Failure::Io)
    })
    .await
    .map_err(|_| Failure::Io)?
}

#[tauri::command]
async fn read_tool(
    current: State<'_, CurrentSession>,
    request: u64,
    index: usize,
    page: usize,
) -> Result<serde_json::Value, Failure> {
    let preview = current_preview(&current, request)?;
    tauri::async_runtime::spawn_blocking(move || {
        serde_json::to_value(preview.tool(index, page)?).map_err(|_| Failure::Io)
    })
    .await
    .map_err(|_| Failure::Io)?
}

#[tauri::command]
async fn search_view(
    current: State<'_, CurrentSession>,
    request: u64,
    view: View,
    query: String,
    cursor: Option<search::Cursor>,
) -> Result<serde_json::Value, Failure> {
    let preview = current_preview(&current, request)?;
    tauri::async_runtime::spawn_blocking(move || {
        serde_json::to_value(preview.search(view, &query, cursor)?).map_err(|_| Failure::Io)
    })
    .await
    .map_err(|_| Failure::Io)?
}

#[tauri::command]
async fn read_image(
    current: State<'_, CurrentSession>,
    request: u64,
    index: usize,
    image: usize,
) -> Result<tauri::ipc::Response, Failure> {
    let preview = current_preview(&current, request)?;
    tauri::async_runtime::spawn_blocking(move || {
        preview.image(index, image).map(tauri::ipc::Response::new)
    })
    .await
    .map_err(|_| Failure::Io)?
}

fn main() {
    tauri::Builder::default()
        .manage(Index::default())
        .manage(CurrentSession::default())
        .invoke_handler(tauri::generate_handler![
            list_sessions,
            read_session,
            read_view,
            read_tool,
            read_image,
            search_view,
            export::copy_block,
            export::copy_source,
            export::save_export,
        ])
        .run(tauri::generate_context!())
        .expect("Session Take could not start");
}
