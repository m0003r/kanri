// SPDX-License-Identifier: Apache-2.0 OR MIT

use serde_json::json;
use std::{fs::File, io::Read, path::PathBuf, sync::Mutex};
use tauri::{AppHandle, Emitter, Manager, Url};

const MAX_DOCUMENT_BYTES: u64 = 8 * 1024 * 1024;

// LaunchServices can open a document before the webview is listening.
pub struct PendingDocuments(Mutex<Option<Vec<PathBuf>>>);

impl Default for PendingDocuments {
    fn default() -> Self {
        Self(Mutex::new(Some(Vec::new())))
    }
}

pub fn open(app: &AppHandle, urls: Vec<Url>) {
    let state = app.state::<PendingDocuments>();
    let mut pending = state.0.lock().unwrap();
    for path in urls.into_iter().filter_map(|url| url.to_file_path().ok()) {
        if path.extension().and_then(|ext| ext.to_str()) != Some("kanri-board") {
            continue;
        }
        if let Some(paths) = pending.as_mut() {
            paths.push(path);
        } else {
            deliver(app, path);
        }
    }
}

#[tauri::command]
pub fn board_documents_ready(app: AppHandle) {
    let state = app.state::<PendingDocuments>();
    let mut pending = state.0.lock().unwrap();
    for path in pending.take().into_iter().flatten() {
        deliver(&app, path);
    }
}

fn deliver(app: &AppHandle, path: PathBuf) {
    let read = || -> Result<serde_json::Value, String> {
        let mut bytes = Vec::new();
        let file = File::open(&path).map_err(|error| error.to_string())?;
        file.take(MAX_DOCUMENT_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
            return Err("Kanri board document exceeds 8 MiB".into());
        }
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())
    };
    let payload = match read() {
        Ok(board) => json!({ "board": board }),
        Err(error) => json!({ "error": error }),
    };
    if let Err(error) = app.emit("kanri-board-document", payload) {
        eprintln!("Could not open Kanri board document: {error}");
    }
}
