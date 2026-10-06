//! HTTP uploads reuse the existing firmware validation and CAN flash sequence.

use std::{
    convert::Infallible,
    io::Write,
    pin::Pin,
    sync::Arc,
    task::{Context as TaskContext, Poll},
    time::Duration,
};

use anyhow::{Context, Result};
use axum::{
    Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Multipart, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use tokio::sync::{Semaphore, mpsc};

use crate::{can::SocketCan, config, firmware, flash::FirmwareFlashManager, types::ConfigFile};

const MAX_UPLOAD: usize = 32 * 1024 * 1024;
type ApiError = (StatusCode, String);

struct Server {
    config: ConfigFile,
    interface: String,
    operation: Arc<Semaphore>,
}

pub fn run(config: ConfigFile, interface: String) -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let state = Arc::new(Server {
            config,
            interface,
            operation: Arc::new(Semaphore::new(1)),
        });
        let app = Router::new()
            .route("/flash", post(flash))
            .route("/status", get(status))
            .layer(DefaultBodyLimit::max(MAX_UPLOAD))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
        println!("Ajax listening on http://{}", listener.local_addr()?);
        axum::serve(listener, app)
            .await
            .context("HTTP server failed")
    })
}

async fn status(State(server): State<Arc<Server>>) -> &'static str {
    if server.operation.available_permits() == 0 {
        "busy"
    } else {
        "idle"
    }
}

struct Upload {
    ecu: String,
    file: tempfile::NamedTempFile,
    filename: String,
    already_in_bootloader: bool,
}

fn bad_request(error: impl std::fmt::Display) -> ApiError {
    (StatusCode::BAD_REQUEST, error.to_string())
}

async fn read_upload(mut multipart: Multipart) -> Result<Upload, ApiError> {
    let mut ecu = None;
    let mut file = None;
    let mut filename = String::from("firmware.elf");
    let mut already_in_bootloader = None;
    while let Some(field) = multipart.next_field().await.map_err(bad_request)? {
        match field.name() {
            Some("ecu") if ecu.is_none() => {
                let value = field.text().await.map_err(bad_request)?;
                if value.trim().is_empty() {
                    return Err(bad_request("ecu must not be empty"));
                }
                ecu = Some(value);
            }
            Some("already_in_bootloader") if already_in_bootloader.is_none() => {
                already_in_bootloader =
                    Some(match field.text().await.map_err(bad_request)?.as_str() {
                        "true" => true,
                        "false" => false,
                        _ => {
                            return Err(bad_request("already_in_bootloader must be true or false"));
                        }
                    });
            }
            Some("file") if file.is_none() => {
                filename = field.file_name().unwrap_or("firmware.elf").to_owned();
                let bytes = field.bytes().await.map_err(bad_request)?;
                if !bytes.starts_with(b"\x7fELF") {
                    return Err(bad_request("file must be an ELF image"));
                }
                // Never use the client-supplied filename as a filesystem path.
                let mut temporary = tempfile::Builder::new()
                    .suffix(".elf")
                    .tempfile()
                    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
                temporary
                    .write_all(&bytes)
                    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
                file = Some(temporary);
            }
            _ => return Err(bad_request("unknown or duplicate multipart field")),
        }
    }
    Ok(Upload {
        ecu: ecu.ok_or_else(|| bad_request("missing ecu field"))?,
        file: file.ok_or_else(|| bad_request("missing file field"))?,
        filename,
        already_in_bootloader: already_in_bootloader.unwrap_or(false),
    })
}

async fn flash(
    State(server): State<Arc<Server>>,
    multipart: Multipart,
) -> Result<Response, ApiError> {
    let permit = server
        .operation
        .clone()
        .try_acquire_owned()
        .map_err(|_| (StatusCode::CONFLICT, "CAN interface is busy".to_owned()))?;
    let upload = tokio::time::timeout(Duration::from_secs(60), read_upload(multipart))
        .await
        .map_err(|_| (StatusCode::REQUEST_TIMEOUT, "upload timed out".to_owned()))??;
    let ecu = config::select_ecu(&server.config, &upload.ecu).map_err(bad_request)?;

    // Validate before starting the response so input/CAN setup failures keep HTTP error codes.
    let interface = server.interface.clone();
    let (image, can, permit) = tokio::task::spawn_blocking(move || {
        let image = firmware::load(upload.file.path()).map_err(bad_request)?;
        let can = SocketCan::open(&interface).map_err(|e| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("Cannot open {interface}: {e:#}"),
            )
        })?;
        Ok::<_, ApiError>((image, can, permit))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;

    let (sender, receiver) = mpsc::unbounded_channel();
    let progress_sender = sender.clone();
    let name = upload.ecu.to_ascii_uppercase();
    println!(
        "{name} firmware update request received: file={:?}, format={}, address=0x{:08X}, size={} bytes, CRC32=0x{:08X}",
        upload.filename,
        image.format,
        image.address,
        image.data.len(),
        image.crc32
    );
    send_event(
        &sender,
        serde_json::json!({
            "type": "firmware", "file": upload.filename, "format": image.format,
            "address": image.address, "size": image.data.len(), "crc32": image.crc32
        }),
    );
    // The worker retains CAN ownership even when the HTTP client disconnects.
    let worker = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        FirmwareFlashManager::new(can, ecu).flash_with_progress(
            &image,
            upload.already_in_bootloader,
            |percent, stage, detail| {
                send_event(
                    &progress_sender,
                    serde_json::json!({
                        "type": "progress", "percent": percent, "stage": stage, "detail": detail
                    }),
                );
                Ok(())
            },
        )
    });
    tokio::spawn(async move {
        let result = match worker.await {
            Ok(result) => result,
            Err(error) => Err(anyhow::anyhow!("Flash worker failed: {error}")),
        };
        match result {
            Ok(()) => {
                let message = format!("{name} firmware verified and activation acknowledged");
                send_event(
                    &sender,
                    serde_json::json!({"type": "complete", "message": message}),
                );
            }
            Err(error) => {
                let message = format!("Flash failed: {error:#}");
                send_event(
                    &sender,
                    serde_json::json!({"type": "error", "message": message}),
                );
            }
        }
    });
    Ok((
        [
            ("content-type", "application/x-ndjson"),
            ("cache-control", "no-cache"),
            ("x-accel-buffering", "no"),
        ],
        Body::from_stream(ProgressStream(receiver)),
    )
        .into_response())
}

fn send_event(sender: &mpsc::UnboundedSender<Bytes>, event: serde_json::Value) {
    // Losing the progress consumer must not abort an ECU update.
    let _ = sender.send(Bytes::from(format!("{event}\n")));
}

struct ProgressStream(mpsc::UnboundedReceiver<Bytes>);

impl futures_core::Stream for ProgressStream {
    type Item = Result<Bytes, Infallible>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
    ) -> Poll<Option<Self::Item>> {
        self.0.poll_recv(context).map(|item| item.map(Ok))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    #[test]
    fn reject_invalid_uploads_and_serialize_operations() {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let server = Arc::new(Server {
                config: ConfigFile { schema_version: 1, supported_bit_rates: vec![500_000], ecus: Default::default() },
                interface: "nonexistent-test-interface".into(),
                operation: Arc::new(Semaphore::new(1)),
            });
            let app = Router::new().route("/flash", post(flash)).route("/status", get(status))
                .layer(DefaultBodyLimit::max(MAX_UPLOAD)).with_state(server.clone());
            let request = |body: &str| Request::post("/flash")
                .header("content-type", "multipart/form-data; boundary=test")
                .body(Body::from(body.to_owned())).unwrap();
            let permit = server.operation.clone().acquire_owned().await.unwrap();
            assert_eq!(status(State(server.clone())).await, "busy");
            let response = app.clone().oneshot(request("--test--\r\n")).await.unwrap();
            assert_eq!(response.status(), StatusCode::CONFLICT);
            drop(permit);
            let response = app.clone().oneshot(request("--test--\r\n")).await.unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let response = app.clone().oneshot(request("--test\r\nContent-Disposition: form-data; name=\"file\"; filename=\"app\"\r\n\r\nnot-elf\r\n--test--\r\n")).await.unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let response = app.oneshot(request("--test\r\nContent-Disposition: form-data; name=\"ecu\"\r\n\r\nunknown\r\n--test\r\nContent-Disposition: form-data; name=\"file\"; filename=\"app\"\r\n\r\n\x7fELFtest\r\n--test--\r\n")).await.unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert_eq!(status(State(server)).await, "idle");
        });
    }
}
