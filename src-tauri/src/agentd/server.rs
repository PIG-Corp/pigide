//! Per-connection handler for the broker.
//!
//! The unix-socket listener (in the binary) accepts connections and hands
//! each one to [`serve_connection`], which loops:
//!
//!   1. Read one NDJSON frame.
//!   2. Decode as [`Request`].
//!   3. Dispatch to the [`Engine`] (or the Hello/Subscribe/Detach
//!      meta-ops handled here directly).
//!   4. Encode the [`Response`] and write it back.
//!
//! When the connection issues `Subscribe`, a background tokio task is
//! spawned that drains the engine's broadcast channel and forwards the
//! events down the same socket as [`Event`] frames. The task ends when
//! the connection closes (sender drop) or when [`Op::Detach`] is sent.
//!
//! Concurrency model:
//! - One handler task per connection (no internal Mutex on the writer).
//! - Engine state is shared (Arc) across all handlers.
//! - Outgoing frames serialize through a single mpsc — Hello/Pong from
//!   the request loop AND Stdout/Exit from the subscribe task funnel
//!   into the same writer task, so frames never interleave mid-line.

use crate::agentd::engine::{Engine, EngineError, EngineEvent, SpawnRequest, DEFAULT_READINESS_TIMEOUT};
use crate::agentd::framing::{read_frame_async, write_frame_async};
use crate::agentd::proto::{
    ErrorCode, Event, HelloResponse, Op, Request, Response, ResponseBody, PROTOCOL_VERSION,
};
use base64::Engine as _;
use std::sync::Arc;
use tokio::io::BufReader;
use tokio::sync::mpsc;

/// Pretty broker build string emitted in `Hello` responses.
pub const BROKER_BUILD: &str = concat!("pigide-agentd ", env!("CARGO_PKG_VERSION"));

/// Outgoing frame, queued to the dedicated writer task.
enum OutFrame {
    Response(Response),
    Event(Event),
}

/// Run one connection to completion. Returns when the peer closes the
/// socket or when the handler hits a fatal protocol error (oversized
/// frame, malformed JSON before Hello, etc.). The engine is unaffected
/// — agents keep running.
pub async fn serve_connection<R, W>(
    engine: Arc<Engine>,
    reader: R,
    writer: W,
) -> std::io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let mut reader = BufReader::new(reader);
    let (tx, rx) = mpsc::channel::<OutFrame>(256);

    // Writer task: serialize and ship each outgoing frame. One task per
    // connection, owns the writer half exclusively.
    let writer_task = tokio::spawn(writer_loop(writer, rx));

    // Subscribe task handle — set on first Subscribe op, dropped on
    // Detach or connection close so the broadcast receiver exits.
    let mut subscribe_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut hello_done = false;

    loop {
        let line = match read_frame_async(&mut reader).await {
            Ok(Some(l)) => l,
            Ok(None) => break, // peer closed
            Err(e) => {
                tracing::warn!("agentd: frame read error: {}", e);
                break;
            }
        };

        let req: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let _ = tx
                    .send(OutFrame::Response(Response {
                        // Frame parse failed — we don't know the id, use 0.
                        id: 0,
                        result: ResponseBody::Error {
                            code: ErrorCode::Invalid,
                            message: format!("decode: {}", e),
                        },
                    }))
                    .await;
                continue;
            }
        };

        // Hello must be first. Anything else before Hello = NoHello error.
        if !hello_done && !matches!(req.op, Op::Hello { .. }) {
            let _ = tx
                .send(OutFrame::Response(Response {
                    id: req.id,
                    result: ResponseBody::Error {
                        code: ErrorCode::NoHello,
                        message: "issue Hello before any other op".into(),
                    },
                }))
                .await;
            continue;
        }

        let resp = match req.op {
            Op::Hello { client_version } => {
                if client_version != PROTOCOL_VERSION {
                    Response {
                        id: req.id,
                        result: ResponseBody::Error {
                            code: ErrorCode::VersionMismatch,
                            message: format!(
                                "client speaks v{}, broker speaks v{}",
                                client_version, PROTOCOL_VERSION
                            ),
                        },
                    }
                } else {
                    hello_done = true;
                    Response {
                        id: req.id,
                        result: ResponseBody::Hello {
                            hello: HelloResponse {
                                broker_version: PROTOCOL_VERSION,
                                broker_build: BROKER_BUILD.into(),
                                broker_pid: std::process::id(),
                            },
                        },
                    }
                }
            }
            Op::Ping => Response {
                id: req.id,
                result: ResponseBody::Pong {
                    uptime_secs: engine.uptime_secs(),
                    live_agents: engine.live_agents(),
                },
            },
            Op::Spawn {
                workspace_id,
                agent_type,
                cwd,
                bin_path,
                argv,
                env,
                reuse_id,
            } => match engine.spawn(SpawnRequest {
                workspace_id,
                agent_type,
                cwd,
                bin_path,
                argv,
                env,
                reuse_id,
                cols: 80,
                rows: 24,
            }) {
                Ok(info) => Response {
                    id: req.id,
                    result: ResponseBody::Spawn { agent: info },
                },
                Err(e) => err_response(req.id, &e),
            },
            Op::Write { agent_id, data_b64 } => {
                match base64::engine::general_purpose::STANDARD.decode(&data_b64) {
                    Ok(bytes) => match engine.write(&agent_id, &bytes, DEFAULT_READINESS_TIMEOUT) {
                        Ok(n) => Response {
                            id: req.id,
                            result: ResponseBody::Write { bytes_written: n },
                        },
                        Err(e) => err_response(req.id, &e),
                    },
                    Err(e) => Response {
                        id: req.id,
                        result: ResponseBody::Error {
                            code: ErrorCode::Invalid,
                            message: format!("data_b64: {}", e),
                        },
                    },
                }
            }
            Op::Resize {
                agent_id,
                cols,
                rows,
            } => match engine.resize(&agent_id, cols, rows) {
                Ok(()) => Response {
                    id: req.id,
                    result: ResponseBody::Resize,
                },
                Err(e) => err_response(req.id, &e),
            },
            Op::Kill { agent_id } => match engine.kill(&agent_id) {
                Ok(()) => Response {
                    id: req.id,
                    result: ResponseBody::Kill,
                },
                Err(e) => err_response(req.id, &e),
            },
            Op::List { workspace_id } => Response {
                id: req.id,
                result: ResponseBody::List {
                    agents: engine.list_workspace(&workspace_id),
                },
            },
            Op::ListAll => Response {
                id: req.id,
                result: ResponseBody::ListAll {
                    agents: engine.list_all(),
                },
            },
            Op::ListPersistedRunning => Response {
                // Broker has no DB — same as ListAll for now. PigIDE
                // bridges this with its own SQLite during migration.
                id: req.id,
                result: ResponseBody::ListPersistedRunning {
                    agents: engine.list_all(),
                },
            },
            Op::LogTail {
                agent_id,
                max_bytes,
            } => match engine.log_tail(&agent_id, max_bytes) {
                Ok(bytes) => Response {
                    id: req.id,
                    result: ResponseBody::LogTail {
                        data_b64: base64::engine::general_purpose::STANDARD.encode(&bytes),
                    },
                },
                Err(e) => err_response(req.id, &e),
            },
            Op::LastStdoutAge { agent_id } => Response {
                id: req.id,
                result: ResponseBody::LastStdoutAge {
                    age_ms: engine
                        .last_stdout_age(&agent_id)
                        .map(|d| d.as_millis() as u64),
                },
            },
            Op::ResetStatuses => Response {
                // No-op on the broker side — the broker is the source of
                // truth, there's nothing to "reset". PigIDE clears its
                // SQLite mirror itself.
                id: req.id,
                result: ResponseBody::ResetStatuses,
            },
            Op::RespawnPersisted { agent_id } => {
                // Broker has no DB to look up persisted argv from. PigIDE
                // resolves the row → SpawnRequest → Op::Spawn{reuse_id}.
                // This op is left here for protocol symmetry; broker
                // returns ok=false (= "I don't know how to respawn this").
                let _ = agent_id;
                Response {
                    id: req.id,
                    result: ResponseBody::RespawnPersisted { ok: false },
                }
            }
            Op::Subscribe => {
                if subscribe_task.is_none() {
                    let mut rx_evt = engine.subscribe();
                    let tx_clone = tx.clone();
                    subscribe_task = Some(tokio::spawn(async move {
                        loop {
                            match rx_evt.recv().await {
                                Ok(EngineEvent::Stdout { agent_id, data }) => {
                                    let frame = OutFrame::Event(Event::Stdout {
                                        agent_id,
                                        data_b64: base64::engine::general_purpose::STANDARD
                                            .encode(&*data),
                                    });
                                    if tx_clone.send(frame).await.is_err() {
                                        break;
                                    }
                                }
                                Ok(EngineEvent::Exit { agent_id }) => {
                                    let frame = OutFrame::Event(Event::Exit { agent_id });
                                    if tx_clone.send(frame).await.is_err() {
                                        break;
                                    }
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                    tracing::warn!(
                                        "agentd: subscriber lagged by {} events; \
                                         scrollback is the recovery path",
                                        n
                                    );
                                    continue;
                                }
                                Err(_) => break,
                            }
                        }
                    }));
                }
                Response {
                    id: req.id,
                    result: ResponseBody::Subscribe,
                }
            }
            Op::Detach => {
                if let Some(t) = subscribe_task.take() {
                    t.abort();
                }
                let _ = tx
                    .send(OutFrame::Response(Response {
                        id: req.id,
                        result: ResponseBody::Detach,
                    }))
                    .await;
                break;
            }
        };

        if tx.send(OutFrame::Response(resp)).await.is_err() {
            break;
        }
    }

    if let Some(t) = subscribe_task.take() {
        t.abort();
    }
    drop(tx);
    let _ = writer_task.await;
    Ok(())
}

fn err_response(id: u64, e: &EngineError) -> Response {
    Response {
        id,
        result: ResponseBody::Error {
            code: e.code(),
            message: e.to_string(),
        },
    }
}

async fn writer_loop<W>(mut writer: W, mut rx: mpsc::Receiver<OutFrame>)
where
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    while let Some(out) = rx.recv().await {
        let json = match &out {
            OutFrame::Response(r) => serde_json::to_string(r),
            OutFrame::Event(e) => serde_json::to_string(e),
        };
        let line = match json {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("agentd: encode failed: {}", e);
                continue;
            }
        };
        if let Err(e) = write_frame_async(&mut writer, &line).await {
            tracing::warn!("agentd: write failed: {}", e);
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::{AsyncBufRead, AsyncWrite};

    async fn write_req<W: AsyncWrite + Unpin>(w: &mut W, req: &Request) {
        let s = serde_json::to_string(req).unwrap();
        write_frame_async(w, &s).await.unwrap();
    }

    async fn read_resp<R: AsyncBufRead + Unpin>(r: &mut R) -> Response {
        let line = read_frame_async(r).await.unwrap().expect("frame");
        serde_json::from_str(&line).unwrap()
    }

    /// Wire up a server in-process via tokio::io::duplex — no real socket.
    fn make_server() -> (
        tokio::io::DuplexStream,
        tokio::task::JoinHandle<()>,
        Arc<Engine>,
        tempfile::TempDir,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let engine = Arc::new(Engine::new(dir.path().to_path_buf()).unwrap());
        let (client, server) = tokio::io::duplex(64 * 1024);
        let (server_r, server_w) = tokio::io::split(server);
        let engine_clone = engine.clone();
        let task = tokio::spawn(async move {
            let _ = serve_connection(engine_clone, server_r, server_w).await;
        });
        (client, task, engine, dir)
    }

    #[tokio::test]
    async fn op_before_hello_rejected() {
        let (client, _task, _engine, _dir) = make_server();
        let (r, w) = tokio::io::split(client);
        let mut br = tokio::io::BufReader::new(r);
        let mut bw = w;

        write_req(
            &mut bw,
            &Request {
                id: 1,
                op: Op::Ping,
            },
        )
        .await;
        let resp = read_resp(&mut br).await;
        assert_eq!(resp.id, 1);
        match resp.result {
            ResponseBody::Error { code, .. } => assert_eq!(code, ErrorCode::NoHello),
            other => panic!("wanted NoHello, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn hello_then_ping_succeeds() {
        let (client, _task, _engine, _dir) = make_server();
        let (r, w) = tokio::io::split(client);
        let mut br = tokio::io::BufReader::new(r);
        let mut bw = w;

        write_req(
            &mut bw,
            &Request {
                id: 1,
                op: Op::Hello {
                    client_version: PROTOCOL_VERSION,
                },
            },
        )
        .await;
        let h = read_resp(&mut br).await;
        match h.result {
            ResponseBody::Hello { hello } => assert_eq!(hello.broker_version, PROTOCOL_VERSION),
            other => panic!("wanted Hello, got {:?}", other),
        }
        write_req(
            &mut bw,
            &Request {
                id: 2,
                op: Op::Ping,
            },
        )
        .await;
        let p = read_resp(&mut br).await;
        match p.result {
            ResponseBody::Pong { live_agents, .. } => assert_eq!(live_agents, 0),
            other => panic!("wanted Pong, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn version_mismatch_returns_error() {
        let (client, _task, _engine, _dir) = make_server();
        let (r, w) = tokio::io::split(client);
        let mut br = tokio::io::BufReader::new(r);
        let mut bw = w;
        write_req(
            &mut bw,
            &Request {
                id: 1,
                op: Op::Hello { client_version: 999 },
            },
        )
        .await;
        let resp = read_resp(&mut br).await;
        match resp.result {
            ResponseBody::Error { code, .. } => assert_eq!(code, ErrorCode::VersionMismatch),
            other => panic!("got {:?}", other),
        }
    }

    #[tokio::test]
    async fn spawn_then_subscribe_streams_stdout_and_exit() {
        let (client, _task, _engine, _dir) = make_server();
        let (r, w) = tokio::io::split(client);
        let mut br = tokio::io::BufReader::new(r);
        let mut bw = w;
        // Hello.
        write_req(
            &mut bw,
            &Request {
                id: 1,
                op: Op::Hello {
                    client_version: PROTOCOL_VERSION,
                },
            },
        )
        .await;
        let _ = read_resp(&mut br).await;

        // Subscribe before spawn so we don't miss any events.
        write_req(
            &mut bw,
            &Request {
                id: 2,
                op: Op::Subscribe,
            },
        )
        .await;
        let s = read_resp(&mut br).await;
        assert!(matches!(s.result, ResponseBody::Subscribe));

        // Spawn a short-lived process.
        write_req(
            &mut bw,
            &Request {
                id: 3,
                op: Op::Spawn {
                    workspace_id: "ws".into(),
                    agent_type: "test".into(),
                    cwd: Some("/tmp".into()),
                    bin_path: "/bin/sh".into(),
                    argv: vec!["-c".into(), "printf hi; sleep 0.05".into()],
                    env: vec![],
                    reuse_id: None,
                },
            },
        )
        .await;
        let sp = read_resp(&mut br).await;
        let agent_id = match sp.result {
            ResponseBody::Spawn { agent } => agent.id,
            other => panic!("wanted Spawn, got {:?}", other),
        };

        // Drain frames for up to 3s; expect at least one Stdout for our
        // agent_id and one Exit. Frames may include other Responses if
        // any background ops slipped in (none in this test).
        let mut got_stdout = false;
        let mut got_exit = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while tokio::time::Instant::now() < deadline && !(got_stdout && got_exit) {
            let line = match tokio::time::timeout(
                Duration::from_millis(500),
                read_frame_async(&mut br),
            )
            .await
            {
                Ok(Ok(Some(l))) => l,
                _ => continue,
            };
            // It can be a Response or an Event; try Event first.
            if let Ok(ev) = serde_json::from_str::<Event>(&line) {
                match ev {
                    Event::Stdout { agent_id: a, .. } if a == agent_id => got_stdout = true,
                    Event::Exit { agent_id: a } if a == agent_id => got_exit = true,
                    _ => {}
                }
            }
        }
        assert!(got_stdout, "did not see Stdout for spawned agent");
        assert!(got_exit, "did not see Exit for spawned agent");
    }

    #[tokio::test]
    async fn detach_closes_connection_without_killing_agents() {
        let (client, task, engine, _dir) = make_server();
        let (r, w) = tokio::io::split(client);
        let mut br = tokio::io::BufReader::new(r);
        let mut bw = w;
        write_req(
            &mut bw,
            &Request {
                id: 1,
                op: Op::Hello {
                    client_version: PROTOCOL_VERSION,
                },
            },
        )
        .await;
        let _ = read_resp(&mut br).await;

        write_req(
            &mut bw,
            &Request {
                id: 2,
                op: Op::Spawn {
                    workspace_id: "ws".into(),
                    agent_type: "t".into(),
                    cwd: None,
                    bin_path: "/bin/sh".into(),
                    argv: vec!["-c".into(), "sleep 1".into()],
                    env: vec![],
                    reuse_id: None,
                },
            },
        )
        .await;
        let _ = read_resp(&mut br).await;

        // Detach. Connection should close cleanly; engine state lives on.
        write_req(
            &mut bw,
            &Request {
                id: 3,
                op: Op::Detach,
            },
        )
        .await;
        let d = read_resp(&mut br).await;
        assert!(matches!(d.result, ResponseBody::Detach));
        // Server task should finish on its own once we drop the client.
        drop(bw);
        drop(br);
        let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
        // Engine still holds the agent.
        assert_eq!(engine.list_all().len(), 1);
    }

    #[tokio::test]
    async fn malformed_json_returns_invalid_error() {
        let (client, _task, _engine, _dir) = make_server();
        let (r, w) = tokio::io::split(client);
        let mut br = tokio::io::BufReader::new(r);
        let mut bw = w;
        write_req(
            &mut bw,
            &Request {
                id: 1,
                op: Op::Hello {
                    client_version: PROTOCOL_VERSION,
                },
            },
        )
        .await;
        let _ = read_resp(&mut br).await;
        // Send raw garbage.
        write_frame_async(&mut bw, "{not json}").await.unwrap();
        let resp = read_resp(&mut br).await;
        match resp.result {
            ResponseBody::Error { code, .. } => assert_eq!(code, ErrorCode::Invalid),
            other => panic!("got {:?}", other),
        }
    }
}
