use std::sync::Arc;

use hub_proto::{devices_changed_notification, Request, Response, MAX_LINE_BYTES};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::sync::{broadcast, watch};

use crate::rpc::dispatch;
use crate::service::{Event, Service};

/// Accept clients until `shutdown` flips to true.
///
/// `first_pipe_instance(true)` makes a stale second daemon fail loudly instead
/// of silently creating a parallel bus.
pub async fn serve(
    pipe_name: &str,
    service: Arc<Service>,
    mut shutdown: watch::Receiver<bool>,
) -> std::io::Result<()> {
    let mut server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(pipe_name)?;
    tracing::info!(pipe = pipe_name, "listening");

    loop {
        tokio::select! {
            biased;
            _ = shutdown.changed() => {
                tracing::info!("shutdown requested");
                break;
            }
            result = server.connect() => {
                result?;
            }
        }

        let connected = server;
        server = ServerOptions::new().create(pipe_name)?;
        let client_service = Arc::clone(&service);
        tracing::debug!("client connected");
        tokio::spawn(async move {
            if let Err(err) = handle_client(connected, client_service).await {
                tracing::debug!(error = %err, "client disconnected");
            }
        });
    }
    Ok(())
}

async fn handle_client(pipe: NamedPipeServer, service: Arc<Service>) -> std::io::Result<()> {
    let (reader, writer) = tokio::io::split(pipe);
    let mut lines = BufReader::new(reader).lines();
    let mut writer = BufWriter::new(writer);
    let mut events = service.subscribe();

    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else { break };
                if line.len() > MAX_LINE_BYTES {
                    tracing::warn!(bytes = line.len(), "request line too large; closing client");
                    break;
                }
                if line.trim().is_empty() {
                    continue;
                }
                let response = match serde_json::from_str::<Request>(&line) {
                    Ok(request) => dispatch(Arc::clone(&service), request).await,
                    Err(err) => Some(Response::parse_error(err.to_string())),
                };
                if let Some(response) = response {
                    write_json(&mut writer, &response).await?;
                }
            }
            event = events.recv() => {
                match event {
                    Ok(Event::DevicesChanged(result)) => {
                        write_json(&mut writer, &devices_changed_notification(&result)).await?;
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(skipped, "client is too slow; dropped events");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
    Ok(())
}

async fn write_json<W, T>(writer: &mut W, value: &T) -> std::io::Result<()>
where
    W: AsyncWriteExt + Unpin,
    T: serde::Serialize,
{
    let mut bytes = serde_json::to_vec(value).map_err(std::io::Error::other)?;
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    writer.flush().await
}
