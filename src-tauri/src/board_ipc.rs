// SPDX-License-Identifier: Apache-2.0 OR MIT

use serde::Serialize;
use serde_json::{json, Value};
use std::{
    fs::{self, DirBuilder},
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::{
        fs::{DirBuilderExt, FileTypeExt, MetadataExt},
        net::{UnixListener, UnixStream},
    },
    path::Path,
    sync::{
        mpsc::{self, SyncSender},
        Mutex,
    },
    thread,
    time::Duration,
};
use tauri::{AppHandle, Emitter, EventTarget, Manager};

const MAX_REQUEST_BYTES: u64 = 1024 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const APPLY_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Default)]
pub struct BoardIpc {
    started: Mutex<bool>,
    pending: Mutex<Option<(u64, SyncSender<Value>)>>,
}

#[derive(Clone, Serialize)]
struct Request {
    id: u64,
    command: Value,
}

fn bind(app_data: &Path) -> io::Result<UnixListener> {
    let dir = app_data.join("ipc");
    if let Err(error) = DirBuilder::new().mode(0o700).create(&dir) {
        if error.kind() != io::ErrorKind::AlreadyExists {
            return Err(error);
        }
    }
    let metadata = fs::symlink_metadata(&dir)?;
    if !metadata.is_dir()
        || metadata.mode() & 0o077 != 0
        || metadata.uid() != fs::metadata(app_data)?.uid()
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Kanri IPC needs a private socket directory",
        ));
    }

    let path = dir.join("kanri.sock");
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_socket() => match UnixStream::connect(&path) {
            Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
                fs::remove_file(&path)?;
            }
            Err(error) => return Err(error),
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "Kanri IPC is already active",
                ))
            }
        },
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Kanri IPC path is not a socket",
            ))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    UnixListener::bind(path)
}

fn read_command(stream: &UnixStream) -> io::Result<Value> {
    let mut reader = BufReader::new(stream.take(MAX_REQUEST_BYTES + 1));
    let mut line = Vec::new();
    reader.read_until(b'\n', &mut line)?;
    if line.len() as u64 > MAX_REQUEST_BYTES || line.last() != Some(&b'\n') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid IPC frame",
        ));
    }
    serde_json::from_slice(&line).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn serve(mut stream: UnixStream, app: &AppHandle, id: u64) -> io::Result<()> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let reply = match read_command(&stream) {
        Ok(command) => {
            let (send, receive) = mpsc::sync_channel(1);
            let state = app.state::<BoardIpc>();
            *state.pending.lock().unwrap() = Some((id, send));
            let sent = app.emit_to(
                EventTarget::webview_window("main"),
                "board-ipc-request",
                Request { id, command },
            );
            let reply = match sent {
                Ok(()) => receive.recv_timeout(APPLY_TIMEOUT).unwrap_or_else(|_| {
                    json!({"ok": false, "code": "unknown_outcome", "error": "Kanri did not confirm the request; check the board"})
                }),
                Err(_) => json!({"ok": false, "code": "unavailable", "error": "Kanri could not receive the request"}),
            };
            state.pending.lock().unwrap().take();
            reply
        }
        Err(_) => {
            json!({"ok": false, "code": "invalid_request", "error": "Expected one newline-terminated JSON request of at most 1 MiB"})
        }
    };
    serde_json::to_writer(&mut stream, &reply)?;
    stream.write_all(b"\n")
}

#[tauri::command]
pub fn start_board_ipc(app: AppHandle) -> Result<(), String> {
    let state = app.state::<BoardIpc>();
    let mut started = state.started.lock().unwrap();
    if *started {
        return Ok(());
    }
    let root = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    let listener = bind(&root).map_err(|error| error.to_string())?;
    let worker_app = app.clone();
    thread::spawn(move || {
        for (index, stream) in listener.incoming().enumerate() {
            if let Ok(stream) = stream {
                let _ = serve(stream, &worker_app, index as u64);
            }
        }
    });
    *started = true;
    Ok(())
}

#[tauri::command]
pub fn finish_board_ipc(id: u64, reply: Value, state: tauri::State<'_, BoardIpc>) {
    let mut pending = state.pending.lock().unwrap();
    if pending.as_ref().is_some_and(|(current, _)| *current == id) {
        if let Some((_, send)) = pending.take() {
            let _ = send.send(reply);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_one_complete_json_frame() {
        let (read, mut write) = UnixStream::pair().unwrap();
        write.write_all(b"{\"op\":\"board.list\"}\n").unwrap();
        assert_eq!(read_command(&read).unwrap(), json!({"op": "board.list"}));
    }

    #[test]
    fn rejects_truncated_and_oversized_frames() {
        for bytes in [
            b"{\"op\":\"board.list\"}".to_vec(),
            vec![b'x'; MAX_REQUEST_BYTES as usize + 1],
        ] {
            let (read, mut write) = UnixStream::pair().unwrap();
            let writer = thread::spawn(move || write.write_all(&bytes).unwrap());
            assert_eq!(
                read_command(&read).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
            writer.join().unwrap();
        }
    }
}
