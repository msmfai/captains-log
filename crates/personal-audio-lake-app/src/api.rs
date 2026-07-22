use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::thread;

use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use personal_audio_lake_contracts::{
    ActionDelivery, ActionKind, ActionProposal, ActionRevisionRequest, ActionUndo, Envelope,
    Message, RecordingAvailable, RecordingReview, ReviewOutcome, SourceAdapter,
    SourceAdapterOutput, SourceCapability, SourceCommand, SourceOperation, SourceState,
    SourceStatus, TranscriptAnnotation, WorkflowDefinition, execute_recording_source,
};
use serde_json::{Value, json};
use uuid::Uuid;

const FRONTEND: &str = include_str!("frontend.html");
const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const MAX_AUDIO_UPLOAD_BYTES: usize = 256 * 1024 * 1024;

#[derive(Clone)]
pub struct Runtime {
    pub bind: String,
    pub state_root: PathBuf,
    pub lake_root: PathBuf,
    pub clickhouse_url: String,
}

pub fn start(runtime: Runtime) -> Result<()> {
    let address: std::net::SocketAddr = runtime
        .bind
        .parse()
        .with_context(|| format!("parse API bind address {}", runtime.bind))?;
    if !address.ip().is_loopback() {
        bail!("local server API must bind to a loopback address");
    }
    let token = ensure_token(&runtime.state_root)?;
    let listener = TcpListener::bind(&runtime.bind)
        .with_context(|| format!("bind local server API to {}", runtime.bind))?;
    thread::Builder::new()
        .name("personal-audio-lake-api".into())
        .spawn(move || {
            for incoming in listener.incoming() {
                match incoming {
                    Ok(stream) => {
                        let runtime = runtime.clone();
                        let token = token.clone();
                        thread::spawn(move || {
                            if let Err(error) = serve(stream, &runtime, &token) {
                                eprintln!("API request failed: {error:#}");
                            }
                        });
                    }
                    Err(error) => eprintln!("API accept failed: {error}"),
                }
            }
        })?;
    Ok(())
}

fn serve(mut stream: TcpStream, runtime: &Runtime, token: &str) -> Result<()> {
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
    let request = Request::read(&mut stream)?;
    let response = if request.path == "/" && request.method == "GET" {
        Response::html(FRONTEND).header(
            "Set-Cookie",
            format!("personal_audio_lake_token={token}; HttpOnly; SameSite=Strict; Path=/"),
        )
    } else if !request.authorized(token) {
        Response::json(401, json!({"error":"unauthorized"}))
    } else {
        match handle(&request, runtime) {
            Ok(response) => response,
            Err(error) => {
                eprintln!("API {} {}: {error:#}", request.method, request.path);
                Response::json(500, json!({"error": error.to_string()}))
            }
        }
    };
    response.write(&mut stream)
}

fn handle(request: &Request, runtime: &Runtime) -> Result<Response> {
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/api/v1/health") => health(runtime),
        ("GET", "/api/v1/inbox") => inbox(runtime),
        ("GET", "/api/v1/recordings") => recordings(runtime, request),
        ("GET", "/api/v1/actions") => actions(runtime),
        ("GET", "/api/v1/events") => events(runtime, request),
        ("POST", "/api/v1/ingest/audio") => ingest_audio_upload(runtime, request),
        ("GET", "/api/v1/services") => list_json_directory(&runtime.state_root.join("services")),
        ("POST", "/api/v1/services") => put_service(runtime, request, None),
        ("GET", "/api/v1/rules") => read_rules(runtime),
        ("PUT", "/api/v1/rules") => write_rules(runtime, request),
        ("POST", "/api/v1/annotations") => annotate(runtime, request),
        ("POST", "/api/v1/integrations/macos/request-calendar-access") => {
            request_macos_access(runtime, "request-calendar-access")
        }
        ("POST", "/api/v1/integrations/macos/request-reminders-access") => {
            request_macos_access(runtime, "request-reminders-access")
        }
        ("GET", "/api/v1/integrations/macos/notifications") => native_notification_status(runtime),
        ("POST", "/api/v1/integrations/macos/request-notification-access") => {
            request_notification_access(runtime)
        }
        _ => dynamic_route(request, runtime),
    }
}

fn inbox(runtime: &Runtime) -> Result<Response> {
    let recordings = clickhouse_json(
        runtime,
        "SELECT toString(a.recording_id) AS recording_id, a.source_id, a.relative_audio_path, \
         a.recorded_at_ms, a.byte_len, ifNull(toString(e.correlation_id), '') AS correlation_id, \
         ifNull(s.transcript, '') AS transcript, \
         ifNull(toString(s.transcript_run_id), '') AS transcript_run_id, \
         ifNull(n.summary, '') AS summary, ifNull(n.hooks_json, '[]') AS notification_hooks, \
         ifNull(s.segment_count, 0) AS segment_count, ifNull(v.outcome, '') AS review_outcome, \
         ifNull(v.note, '') AS review_note, ifNull(v.reviewed_at_ms, 0) AS reviewed_at_ms \
         FROM (SELECT recording_id, argMax(source_id, recorded_at) AS source_id, \
         argMax(relative_path, recorded_at) AS relative_audio_path, \
         max(toUnixTimestamp64Milli(recorded_at)) AS recorded_at_ms, \
         argMax(byte_len, recorded_at) AS byte_len, \
         argMax(introducing_event_id, recorded_at) AS introducing_event_id \
         FROM personal_audio_lake.audio_objects GROUP BY recording_id) AS a \
         ANY LEFT JOIN personal_audio_lake.ingest_events AS e ON a.introducing_event_id=e.event_id \
         LEFT JOIN (SELECT recording_id, argMax(run_id, completed_at) AS transcript_run_id, \
         arrayStringConcat(arraySort(groupArray((ordinal, text))).2, ' ') AS transcript, \
         count() AS segment_count FROM personal_audio_lake.transcript_segments GROUP BY recording_id) AS s \
         ON a.recording_id=s.recording_id LEFT JOIN (SELECT recording_id, argMax(summary, delivered_at) AS summary, \
         argMax(hooks_json, delivered_at) AS hooks_json FROM personal_audio_lake.notification_deliveries GROUP BY recording_id) AS n \
         ON a.recording_id=n.recording_id LEFT JOIN (SELECT recording_id, argMax(outcome, reviewed_at) AS outcome, \
         argMax(note, reviewed_at) AS note, max(toUnixTimestamp64Milli(reviewed_at)) AS reviewed_at_ms \
         FROM personal_audio_lake.recording_reviews GROUP BY recording_id) AS v ON a.recording_id=v.recording_id \
         ORDER BY recorded_at_ms DESC LIMIT 250 FORMAT JSON",
    )?;
    let mut actions = clickhouse_json(
        runtime,
        "SELECT toString(f.action_id) AS action_id, toString(f.recording_id) AS recording_id, \
         f.kind, f.title, ifNull(toUnixTimestamp64Milli(f.due_at), 0) AS due_at_ms, f.confidence, \
         f.evidence_json, f.extractor_version, \
         if(empty(r.decision), 'pending', r.decision) AS decision, ifNull(r.reason, '') AS review_reason, \
         ifNull(d.target, '') AS delivered_target FROM (SELECT action_id, \
         argMax(recording_id, observed_at) AS recording_id, argMax(kind, observed_at) AS kind, \
         argMax(title, observed_at) AS title, argMax(due_at, observed_at) AS due_at, \
         argMax(confidence, observed_at) AS confidence, \
         argMax(evidence_json, observed_at) AS evidence_json, \
         argMax(extractor_version, observed_at) AS extractor_version, \
         max(observed_at) AS latest_observed_at FROM personal_audio_lake.action_facts GROUP BY action_id) AS f \
         LEFT JOIN (SELECT action_id, argMax(decision, reviewed_at) AS decision, \
         argMax(reason, reviewed_at) AS reason FROM personal_audio_lake.action_reviews GROUP BY action_id) AS r \
         ON f.action_id=r.action_id \
         LEFT JOIN (SELECT action_id, argMax(target, delivered_at) AS target \
         FROM personal_audio_lake.action_deliveries GROUP BY action_id) AS d ON f.action_id=d.action_id \
         ORDER BY f.latest_observed_at DESC LIMIT 1000 FORMAT JSON",
    )?;
    let currently_undone = manifest_currently_undone_actions(runtime)?;
    if let Some(values) = actions.get_mut("data").and_then(Value::as_array_mut) {
        for action in values {
            let action_id = action
                .get("action_id")
                .and_then(Value::as_str)
                .and_then(|value| Uuid::parse_str(value).ok());
            if action_id.is_some_and(|id| currently_undone.contains(&id)) {
                action["decision"] = json!("removed");
                action["delivered_target"] = json!("");
            }
        }
    }
    let failures = manifest_failures(runtime)?;
    let workflow_runs = manifest_workflow_runs(runtime)?;
    Ok(Response::json(
        200,
        json!({
            "recordings":recordings.get("data").cloned().unwrap_or_else(|| json!([])),
            "actions":actions.get("data").cloned().unwrap_or_else(|| json!([])),
            "workflow_runs":workflow_runs,
            "failures":failures
        }),
    ))
}

fn manifest_currently_undone_actions(runtime: &Runtime) -> Result<HashSet<Uuid>> {
    let directory = runtime.state_root.join("calendar/undone");
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(HashSet::new()),
        Err(error) => return Err(error.into()),
    };
    Ok(entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.path().file_stem()?.to_str()?.parse().ok())
        .collect())
}

#[cfg(test)]
fn currently_undone_actions(reader: impl BufRead) -> Result<HashSet<Uuid>> {
    let mut undone = HashSet::new();
    for line in reader.lines() {
        let envelope = Envelope::from_json(&line?)?;
        match envelope.message {
            Message::ActionUndone(receipt) => {
                undone.insert(receipt.action_id);
            }
            Message::ActionDelivered(receipt) => {
                undone.remove(&receipt.action_id);
            }
            _ => {}
        }
    }
    Ok(undone)
}

fn manifest_workflow_runs(runtime: &Runtime) -> Result<Vec<Value>> {
    let directory = runtime.state_root.join("workflow-runs");
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let undone_actions = manifest_currently_undone_actions(runtime)?;
    let mut values = Vec::new();
    for entry in entries {
        let directory = entry?.path();
        let Ok(bytes) = fs::read(directory.join("record.json")) else {
            continue;
        };
        let mut value: Value = serde_json::from_slice(&bytes)?;
        value["confirmed"] = json!(directory.join("confirmed").exists());
        let mut undone = Vec::new();
        if let Some(steps) = value.get("steps").and_then(Value::as_array) {
            for step in steps {
                let action_id = step
                    .get("receipt")
                    .and_then(Value::as_str)
                    .and_then(|receipt| receipt.strip_prefix("queued Mac action "))
                    .and_then(|id| Uuid::parse_str(id).ok());
                if action_id.is_some_and(|id| undone_actions.contains(&id)) {
                    undone.push(json!({
                        "transition_id":step.get("selection").and_then(|selection| selection.get("transition_id")).cloned().unwrap_or(Value::Null),
                        "receipt":"Removed from the Mac"
                    }));
                }
            }
        }
        let undo_directory = directory.join("undone");
        if let Ok(entries) = fs::read_dir(undo_directory) {
            for entry in entries.flatten() {
                if let Ok(bytes) = fs::read(entry.path())
                    && let Ok(marker) = serde_json::from_slice::<Value>(&bytes)
                {
                    undone.push(marker);
                }
            }
        }
        value["undone_transitions"] = Value::Array(undone);
        values.push(value);
    }
    values.sort_by_key(|value| {
        std::cmp::Reverse(
            value
                .get("completed_at_ms")
                .and_then(Value::as_i64)
                .unwrap_or_default(),
        )
    });
    Ok(values)
}

fn manifest_failures(runtime: &Runtime) -> Result<Vec<Value>> {
    let mut event_recordings = HashMap::<Uuid, Uuid>::new();
    let mut action_recordings = HashMap::<Uuid, Uuid>::new();
    let mut latest_success = HashMap::<Uuid, i64>::new();
    let mut failures = Vec::<(Uuid, i64, Value)>::new();
    for envelope in recent_manifest_envelopes(runtime, 8 * 1024 * 1024)? {
        match &envelope.message {
            Message::TranscriptCompleted(run) => {
                event_recordings.insert(envelope.id, run.recording_id);
            }
            Message::ActionProposed(action) => {
                action_recordings.insert(action.action_id, action.recording_id);
                event_recordings.insert(envelope.id, action.recording_id);
            }
            Message::ActionApproved { action_id } => {
                if let Some(recording_id) = action_recordings.get(action_id) {
                    event_recordings.insert(envelope.id, *recording_id);
                }
            }
            Message::DerivationCompleted(completed) => {
                latest_success.insert(completed.recording_id, envelope.emitted_at_ms);
                event_recordings.insert(envelope.id, completed.recording_id);
            }
            Message::NotificationDelivered(delivery) => {
                latest_success.insert(delivery.recording_id, envelope.emitted_at_ms);
                event_recordings.insert(envelope.id, delivery.recording_id);
            }
            Message::Failure(failure) if failure.code != "no_action_candidates" => {
                if let Some(recording_id) = failure
                    .failed_message_id
                    .and_then(|event_id| event_recordings.get(&event_id).copied())
                {
                    failures.push((
                        recording_id,
                        envelope.emitted_at_ms,
                        json!({
                            "recording_id":recording_id,
                            "emitted_at_ms":envelope.emitted_at_ms,
                            "worker":format!("{:?}", failure.worker).to_lowercase(),
                            "code":failure.code,
                            "detail":failure.detail
                        }),
                    ));
                }
            }
            _ => {}
        }
    }
    failures.retain(|(recording_id, emitted_at_ms, _)| {
        latest_success
            .get(recording_id)
            .is_none_or(|success| emitted_at_ms > success)
    });
    failures.sort_by_key(|(_, emitted_at_ms, _)| std::cmp::Reverse(*emitted_at_ms));
    let mut seen = std::collections::HashSet::new();
    failures.retain(|(recording_id, _, value)| {
        let code = value
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or_default();
        seen.insert((*recording_id, code.to_owned()))
    });
    Ok(failures
        .into_iter()
        .take(1_000)
        .map(|(_, _, value)| value)
        .collect())
}

fn recent_manifest_envelopes(runtime: &Runtime, maximum_bytes: u64) -> Result<Vec<Envelope>> {
    let path = runtime.lake_root.join("manifests/events.ndjson");
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let length = file.metadata()?.len();
    let start = length.saturating_sub(maximum_bytes);
    file.seek(SeekFrom::Start(start))?;
    let mut reader = BufReader::new(file);
    if start > 0 {
        let mut partial = String::new();
        reader.read_line(&mut partial)?;
    }
    let mut envelopes = Vec::new();
    for line in reader.lines() {
        envelopes.push(Envelope::from_json(&line?)?);
    }
    Ok(envelopes)
}

fn dynamic_route(request: &Request, runtime: &Runtime) -> Result<Response> {
    let parts: Vec<_> = request.path.trim_matches('/').split('/').collect();
    match parts.as_slice() {
        ["api", "v1", "recordings", recording_id] if request.method == "GET" => {
            recording(runtime, recording_id)
        }
        ["api", "v1", "audio", recording_id] if request.method == "GET" => {
            audio(runtime, recording_id)
        }
        ["api", "v1", "actions", action_id, "approve"] if request.method == "POST" => {
            let action_id = Uuid::parse_str(action_id)?;
            enqueue(
                runtime,
                Envelope::root(Message::ActionApproved { action_id }),
            )?;
            Ok(Response::json(
                202,
                json!({"status":"queued","action_id":action_id}),
            ))
        }
        ["api", "v1", "actions", action_id, "reject"] if request.method == "POST" => {
            let action_id = Uuid::parse_str(action_id)?;
            let body: Value = serde_json::from_slice(&request.body)?;
            let reason = body
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("rejected by user")
                .to_owned();
            enqueue(
                runtime,
                Envelope::root(Message::ActionRejected { action_id, reason }),
            )?;
            Ok(Response::json(
                202,
                json!({"status":"queued","action_id":action_id}),
            ))
        }
        ["api", "v1", "actions", action_id, "edit"] if request.method == "POST" => {
            edit_action(runtime, action_id, request)
        }
        ["api", "v1", "actions", action_id, "delivery"] if request.method == "POST" => {
            report_action_delivery(runtime, action_id, request)
        }
        ["api", "v1", "actions", action_id, "external-undo"] if request.method == "POST" => {
            report_action_undo(runtime, action_id, request)
        }
        ["api", "v1", "inbox", recording_id, "review"] if request.method == "POST" => {
            review_recording(runtime, recording_id, request)
        }
        ["api", "v1", "services", service_id]
            if request.method == "PUT" || request.method == "DELETE" =>
        {
            if request.method == "DELETE" {
                validate_id(service_id)?;
                let path = runtime
                    .state_root
                    .join("services")
                    .join(format!("{service_id}.json"));
                if path.exists() {
                    fs::remove_file(path)?;
                }
                Ok(Response::json(200, json!({"status":"deleted"})))
            } else {
                put_service(runtime, request, Some(service_id))
            }
        }
        _ => Ok(Response::json(404, json!({"error":"not found"}))),
    }
}

fn report_action_delivery(
    runtime: &Runtime,
    action_id: &str,
    request: &Request,
) -> Result<Response> {
    let action_id = Uuid::parse_str(action_id)?;
    let body: Value = serde_json::from_slice(&request.body)?;
    let target = receipt_field(&body, "target", 128)?;
    let external_id = receipt_field(&body, "external_id", 512)?;
    let delivered_at_ms = body
        .get("delivered_at_ms")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("delivered_at_ms is required"))?;
    let receipt = ActionDelivery {
        action_id,
        external_id,
        delivered_at_ms,
        target,
    };
    enqueue(
        runtime,
        Envelope::root(Message::ActionDelivered(receipt.clone())),
    )?;
    Ok(Response::json(202, serde_json::to_value(receipt)?))
}

fn report_action_undo(runtime: &Runtime, action_id: &str, request: &Request) -> Result<Response> {
    let action_id = Uuid::parse_str(action_id)?;
    let body: Value = serde_json::from_slice(&request.body)?;
    let target = receipt_field(&body, "target", 128)?;
    let undone_at_ms = body
        .get("undone_at_ms")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("undone_at_ms is required"))?;
    let receipt = ActionUndo {
        action_id,
        target,
        undone_at_ms,
    };
    enqueue(
        runtime,
        Envelope::root(Message::ActionUndone(receipt.clone())),
    )?;
    Ok(Response::json(202, serde_json::to_value(receipt)?))
}

fn receipt_field(body: &Value, name: &str, maximum: usize) -> Result<String> {
    let value = body
        .get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .ok_or_else(|| anyhow!("{name} is required"))?;
    if value.is_empty() || value.chars().count() > maximum || value.chars().any(char::is_control) {
        bail!("{name} must contain 1 to {maximum} non-control characters");
    }
    Ok(value.to_owned())
}

fn health(runtime: &Runtime) -> Result<Response> {
    let storage = clickhouse_json(
        runtime,
        "SELECT (SELECT uniq(recording_id) FROM personal_audio_lake.audio_objects) AS recordings, \
         (SELECT count() FROM personal_audio_lake.transcript_segments) AS transcript_segments, \
         (SELECT uniq(action_id) FROM personal_audio_lake.action_facts) AS actions FORMAT JSON",
    );
    let (counts, storage_status, storage_error) = match storage {
        Ok(value) => (
            value.get("data").cloned().unwrap_or_else(|| json!([])),
            "ready",
            Value::Null,
        ),
        Err(error) => (json!([]), "unavailable", Value::String(error.to_string())),
    };
    let status_path = runtime.state_root.join("status/events.ndjson");
    let status_text = fs::read_to_string(status_path).unwrap_or_default();
    let (workers, workers_ready) = worker_readiness(&status_text);
    let latest = status_text
        .lines()
        .rev()
        .take(30)
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect::<Vec<_>>();
    let status = if storage_status == "ready" && workers_ready {
        "ready"
    } else {
        "degraded"
    };
    Ok(Response::json(
        200,
        json!({
            "status":status,
            "server_time_ms":Utc::now().timestamp_millis(),
            "api_version":1,
            "lake_root":runtime.lake_root,
            "counts":counts,
            "storage":{
                "status":storage_status,
                "error":storage_error
            },
            "workers":workers,
            "recent_events":latest
        }),
    ))
}

fn worker_readiness(status_text: &str) -> (Vec<Value>, bool) {
    const REQUIRED: [&str; 8] = [
        "source",
        "transfer_ingress",
        "lake",
        "transcribe",
        "derive",
        "calendar",
        "notify",
        "services",
    ];
    let mut latest = HashMap::<String, Value>::new();
    for line in status_text.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value.pointer("/event/type").and_then(Value::as_str) != Some("worker_health") {
            continue;
        }
        let Some(role) = value.pointer("/event/payload/role").and_then(Value::as_str) else {
            continue;
        };
        latest.insert(role.to_owned(), value["event"]["payload"].clone());
    }
    let workers = REQUIRED
        .iter()
        .map(|role| {
            latest.get(*role).cloned().unwrap_or_else(|| {
                json!({"role":role,"state":"starting","detail":"worker has not reported readiness","backlog":0})
            })
        })
        .collect::<Vec<_>>();
    let ready = workers
        .iter()
        .all(|worker| worker.get("state").and_then(Value::as_str) == Some("ready"));
    (workers, ready)
}

fn recordings(runtime: &Runtime, request: &Request) -> Result<Response> {
    let search = request.query.get("q").map(String::as_str).unwrap_or("");
    let filter = if search.trim().is_empty() {
        String::new()
    } else {
        format!(
            "WHERE positionCaseInsensitiveUTF8(ifNull(s.transcript, ''), '{}') > 0",
            sql_string(search)
        )
    };
    let query = format!(
        "SELECT toString(a.recording_id) AS recording_id, a.source_id, \
         a.relative_path, a.recorded_at_ms, a.byte_len, ifNull(s.transcript, '') AS transcript, \
         ifNull(s.segment_count, 0) AS segment_count, ifNull(n.summary, '') AS summary, \
         ifNull(n.hooks_json, '[]') AS notification_hooks, ifNull(v.outcome, '') AS review_outcome \
         FROM (SELECT recording_id, \
         argMax(source_id, recorded_at) AS source_id, argMax(relative_path, recorded_at) AS relative_path, \
         max(toUnixTimestamp64Milli(recorded_at)) AS recorded_at_ms, \
         argMax(byte_len, recorded_at) AS byte_len FROM personal_audio_lake.audio_objects GROUP BY recording_id) AS a \
         LEFT JOIN (SELECT recording_id, \
         arrayStringConcat(arraySort(groupArray((ordinal, text))).2, ' ') AS transcript, \
         count() AS segment_count FROM personal_audio_lake.transcript_segments GROUP BY recording_id) AS s \
         ON a.recording_id=s.recording_id LEFT JOIN (SELECT recording_id, \
         argMax(summary, delivered_at) AS summary, argMax(hooks_json, delivered_at) AS hooks_json \
         FROM personal_audio_lake.notification_deliveries GROUP BY recording_id) AS n \
         ON a.recording_id=n.recording_id LEFT JOIN (SELECT recording_id, \
         argMax(outcome, reviewed_at) AS outcome FROM personal_audio_lake.recording_reviews \
         GROUP BY recording_id) AS v ON a.recording_id=v.recording_id \
         {filter} ORDER BY recorded_at_ms DESC LIMIT 250 FORMAT JSON"
    );
    Ok(Response::json(200, clickhouse_json(runtime, &query)?))
}

fn recording(runtime: &Runtime, recording_id: &str) -> Result<Response> {
    let id = Uuid::parse_str(recording_id)?;
    let query = format!(
        "SELECT toString(s.recording_id) AS recording_id, toString(s.run_id) AS run_id, s.ordinal, \
         s.start_ms, s.end_ms, s.speaker, s.raw_text, a.replacement_text, s.confidence, s.language, \
         s.model, s.model_version, s.completed_at_ms FROM (SELECT recording_id, ordinal, \
         argMax(run_id, completed_at) AS run_id, argMax(start_ms, completed_at) AS start_ms, \
         argMax(end_ms, completed_at) AS end_ms, argMax(speaker, completed_at) AS speaker, \
         argMax(text, completed_at) AS raw_text, argMax(confidence, completed_at) AS confidence, \
         argMax(language, completed_at) AS language, argMax(model, completed_at) AS model, \
         argMax(model_version, completed_at) AS model_version, max(toUnixTimestamp64Milli(completed_at)) AS completed_at_ms \
         FROM personal_audio_lake.transcript_segments WHERE recording_id=toUUID('{id}') GROUP BY recording_id, ordinal) AS s \
         LEFT JOIN personal_audio_lake.transcript_annotations AS a FINAL ON s.recording_id=a.recording_id \
         AND s.run_id=a.transcript_run_id AND s.ordinal=a.ordinal ORDER BY s.ordinal FORMAT JSON"
    );
    Ok(Response::json(200, clickhouse_json(runtime, &query)?))
}

fn actions(runtime: &Runtime) -> Result<Response> {
    let query = "SELECT toString(f.action_id) AS action_id, toString(f.recording_id) AS recording_id, \
        toString(f.transcript_run_id) AS transcript_run_id, f.kind, f.title, \
        ifNull(toUnixTimestamp64Milli(f.due_at), 0) AS due_at_ms, f.timezone, f.confidence, \
        f.evidence_json, f.extractor_version, if(empty(r.decision), 'pending', r.decision) AS decision, \
        ifNull(r.reason, '') AS review_reason, ifNull(d.target, '') AS delivered_target \
        FROM (SELECT action_id, argMax(recording_id, observed_at) AS recording_id, \
        argMax(transcript_run_id, observed_at) AS transcript_run_id, argMax(kind, observed_at) AS kind, \
        argMax(title, observed_at) AS title, argMax(due_at, observed_at) AS due_at, \
        argMax(timezone, observed_at) AS timezone, argMax(confidence, observed_at) AS confidence, \
        argMax(evidence_json, observed_at) AS evidence_json, \
        argMax(extractor_version, observed_at) AS extractor_version, \
        max(observed_at) AS latest_observed_at FROM personal_audio_lake.action_facts GROUP BY action_id) AS f \
        LEFT JOIN (SELECT action_id, argMax(decision, reviewed_at) AS decision, \
        argMax(reason, reviewed_at) AS reason FROM personal_audio_lake.action_reviews GROUP BY action_id) AS r \
        ON f.action_id=r.action_id \
        LEFT JOIN (SELECT action_id, argMax(target, delivered_at) AS target \
        FROM personal_audio_lake.action_deliveries GROUP BY action_id) AS d ON f.action_id=d.action_id \
        ORDER BY f.latest_observed_at DESC LIMIT 500 FORMAT JSON";
    Ok(Response::json(200, clickhouse_json(runtime, query)?))
}

fn events(runtime: &Runtime, request: &Request) -> Result<Response> {
    let after = request
        .query
        .get("after")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let limit = request
        .query
        .get("limit")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(100)
        .min(1000);
    let path = runtime.lake_root.join("manifests/events.ndjson");
    let file = fs::File::open(path)?;
    let values = BufReader::new(file)
        .lines()
        .skip(after)
        .take(limit)
        .map(|line| Ok(serde_json::from_str::<Value>(&line?)?))
        .collect::<Result<Vec<_>>>()?;
    Ok(Response::json(
        200,
        json!({"cursor":after + values.len(),"events":values}),
    ))
}

fn audio(runtime: &Runtime, recording_id: &str) -> Result<Response> {
    let id = Uuid::parse_str(recording_id)?;
    let query = format!(
        "SELECT relative_path FROM personal_audio_lake.audio_objects WHERE recording_id=toUUID('{id}') LIMIT 1 FORMAT JSON"
    );
    let value = clickhouse_json(runtime, &query)?;
    let relative = value
        .pointer("/data/0/relative_path")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("recording has no audio object"))?;
    let relative_path = Path::new(relative);
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
        || !relative.starts_with("objects/audio/")
    {
        bail!("invalid stored audio path");
    }
    let path = runtime.lake_root.join(relative_path);
    let content_type = match path.extension().and_then(|value| value.to_str()) {
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("m4a") | Some("aac") => "audio/mp4",
        _ => "application/octet-stream",
    };
    Ok(Response::bytes(200, content_type, fs::read(path)?))
}

fn annotate(runtime: &Runtime, request: &Request) -> Result<Response> {
    let body: Value = serde_json::from_slice(&request.body)?;
    let recording_id = json_uuid(&body, "recording_id")?;
    let transcript_run_id = json_uuid(&body, "transcript_run_id")?;
    let ordinal = body
        .get("ordinal")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("ordinal is required"))? as u32;
    let replacement_text = body
        .get("replacement_text")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("replacement_text is required"))?
        .trim()
        .to_owned();
    let reason = body
        .get("reason")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let identity = format!("{recording_id}:{transcript_run_id}:{ordinal}:{replacement_text}");
    let annotation = TranscriptAnnotation {
        annotation_id: Uuid::new_v5(
            &personal_audio_lake_contracts::RECORDING_NAMESPACE,
            identity.as_bytes(),
        ),
        recording_id,
        transcript_run_id,
        ordinal,
        replacement_text,
        reason,
        annotated_at_ms: Utc::now().timestamp_millis(),
    };
    enqueue(
        runtime,
        Envelope::root(Message::TranscriptAnnotated(annotation.clone())),
    )?;
    Ok(Response::json(202, serde_json::to_value(annotation)?))
}

fn request_macos_access(runtime: &Runtime, permission_command: &str) -> Result<Response> {
    let commands = runtime.state_root.join("ui-commands");
    fs::create_dir_all(&commands)?;
    fs::write(
        commands.join(permission_command),
        Utc::now().timestamp_millis().to_string(),
    )?;
    Ok(Response::json(
        202,
        json!({
            "status":"queued",
            "permission":permission_command,
            "managed_by":"com.smfmarin.personal-audio-lake"
        }),
    ))
}

fn native_notification_status(runtime: &Runtime) -> Result<Response> {
    let settings = fs::read(runtime.state_root.join("notifications/status.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_else(|| {
            json!({
                "authorization":"not_determined",
                "managed_by":"com.smfmarin.personal-audio-lake"
            })
        });
    Ok(Response::json(200, settings))
}

fn request_notification_access(runtime: &Runtime) -> Result<Response> {
    let commands = runtime.state_root.join("ui-commands");
    fs::create_dir_all(&commands)?;
    fs::write(
        commands.join("request-notification-access"),
        Utc::now().timestamp_millis().to_string(),
    )?;
    Ok(Response::json(
        202,
        json!({"status":"queued","permission":"notifications","managed_by":"com.smfmarin.personal-audio-lake"}),
    ))
}

fn review_recording(runtime: &Runtime, recording_id: &str, request: &Request) -> Result<Response> {
    let recording_id = Uuid::parse_str(recording_id)?;
    let body: Value = serde_json::from_slice(&request.body)?;
    let outcome = match body.get("outcome").and_then(Value::as_str) {
        Some("confirmed") => ReviewOutcome::Confirmed,
        Some("needs_attention") => ReviewOutcome::NeedsAttention,
        _ => bail!("outcome must be confirmed or needs_attention"),
    };
    let note = body
        .get("note")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|note| !note.is_empty())
        .map(str::to_owned);
    if outcome == ReviewOutcome::NeedsAttention && note.is_none() {
        bail!("a note is required when flagging a mismatch");
    }
    let review = RecordingReview {
        review_id: Uuid::new_v4(),
        recording_id,
        outcome,
        note,
        reviewed_at_ms: Utc::now().timestamp_millis(),
    };
    enqueue(
        runtime,
        Envelope::root(Message::RecordingReviewed(review.clone())),
    )?;
    Ok(Response::json(202, serde_json::to_value(review)?))
}

fn edit_action(runtime: &Runtime, action_id: &str, request: &Request) -> Result<Response> {
    let action_id = Uuid::parse_str(action_id)?;
    let path = runtime
        .state_root
        .join("calendar/pending")
        .join(format!("{action_id}.json"));
    let mut proposal: ActionProposal = serde_json::from_slice(
        &fs::read(&path).with_context(|| format!("no pending action {action_id}"))?,
    )?;
    let body: Value = serde_json::from_slice(&request.body)?;
    if let Some(title) = body.get("title").and_then(Value::as_str) {
        let title = title.trim();
        if title.is_empty() || title.chars().count() > 300 {
            bail!("title must contain 1 to 300 characters");
        }
        proposal.title = title.to_owned();
    }
    if body.get("due_at_ms").is_some() {
        proposal.due_at_ms = body.get("due_at_ms").and_then(Value::as_i64);
    }
    if let Some(kind) = body.get("kind").and_then(Value::as_str) {
        proposal.kind = match kind {
            "todo" => ActionKind::Todo,
            "reminder" => ActionKind::Reminder,
            "calendar" | "calendar_event" => ActionKind::CalendarEvent,
            _ => bail!("kind must be todo, reminder, or calendar_event"),
        };
    }
    enqueue(
        runtime,
        Envelope::root(Message::ActionRevisionRequested(ActionRevisionRequest {
            action_id,
            kind: proposal.kind,
            title: proposal.title.clone(),
            due_at_ms: proposal.due_at_ms,
            requested_at_ms: Utc::now().timestamp_millis(),
        })),
    )?;
    Ok(Response::json(202, serde_json::to_value(proposal)?))
}

fn put_service(runtime: &Runtime, request: &Request, path_id: Option<&str>) -> Result<Response> {
    let mut value: Value = serde_json::from_slice(&request.body)?;
    let id = path_id
        .map(str::to_owned)
        .or_else(|| value.get("id").and_then(Value::as_str).map(str::to_owned))
        .ok_or_else(|| anyhow!("service id is required"))?;
    validate_id(&id)?;
    value["id"] = Value::String(id.clone());
    validate_service(&value)?;
    let directory = runtime.state_root.join("services");
    fs::create_dir_all(&directory)?;
    atomic_json(&directory.join(format!("{id}.json")), &value)?;
    Ok(Response::json(200, value))
}

fn validate_service(value: &Value) -> Result<()> {
    if let Ok(workflow) = serde_json::from_value::<WorkflowDefinition>(value.clone()) {
        if workflow.initial_state_id != "message_ingested" {
            bail!("workflow initial state must be message_ingested");
        }
        if workflow.states.is_empty()
            || !workflow
                .states
                .iter()
                .any(|state| state.id == workflow.initial_state_id)
        {
            bail!("workflow must contain its initial state");
        }
        for state in &workflow.states {
            for transition in &state.transitions {
                if !workflow
                    .states
                    .iter()
                    .any(|candidate| candidate.id == transition.destination_state_id)
                {
                    bail!(
                        "transition {} points to missing state {}",
                        transition.id,
                        transition.destination_state_id
                    );
                }
            }
        }
        return Ok(());
    }
    let adapter = value
        .pointer("/adapter/type")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("adapter.type is required"))?;
    if !matches!(adapter, "shortcut" | "webhook") {
        bail!("adapter.type must be shortcut or webhook");
    }
    let subscriptions = value
        .get("subscriptions")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("subscriptions must be an array"))?;
    if subscriptions.is_empty() || !subscriptions.iter().all(Value::is_string) {
        bail!("subscriptions must contain message type strings");
    }
    Ok(())
}

fn read_rules(runtime: &Runtime) -> Result<Response> {
    let path = runtime.state_root.join("rules.json");
    if !path.exists() {
        atomic_json(&path, &default_rules())?;
    }
    Ok(Response::bytes(200, "application/json", fs::read(path)?))
}

fn write_rules(runtime: &Runtime, request: &Request) -> Result<Response> {
    let value: Value = serde_json::from_slice(&request.body)?;
    let rules = value
        .get("rules")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("rules array is required"))?;
    for rule in rules {
        let mode = rule.get("mode").and_then(Value::as_str).unwrap_or("");
        if !matches!(mode, "explicit" | "hybrid" | "implicit") {
            bail!("rule mode must be explicit, hybrid, or implicit");
        }
    }
    atomic_json(&runtime.state_root.join("rules.json"), &value)?;
    Ok(Response::json(200, value))
}

fn default_rules() -> Value {
    json!({"semantic_model":"mistral-small3.2:latest","ollama_url":"http://127.0.0.1:11434","rules":[
        {"id":"reminders","enabled":true,"mode":"explicit","phrases":["remind me to","remember to"],"threshold":0.78,"action":"macos.reminder","approval":"auto_when_unambiguous"},
        {"id":"todos","enabled":true,"mode":"explicit","phrases":["add to my todo","add to my to do","todo"],"threshold":0.78,"action":"macos.todo","approval":"auto_when_unambiguous"},
        {"id":"calendar","enabled":true,"mode":"hybrid","phrases":["for my calendar add","calendar add","schedule"],"threshold":0.78,"action":"macos.calendar","approval":"auto_when_unambiguous"},
        {"id":"implicit-commitments","enabled":false,"mode":"implicit","phrases":[],"threshold":0.9,"action":"proposal","approval":"auto_when_unambiguous"}
    ]})
}

fn list_json_directory(directory: &Path) -> Result<Response> {
    fs::create_dir_all(directory)?;
    let mut values = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.path().extension().and_then(|value| value.to_str()) == Some("json") {
            values.push(serde_json::from_slice::<Value>(&fs::read(entry.path())?)?);
        }
    }
    Ok(Response::json(200, json!({"services":values})))
}

fn enqueue(runtime: &Runtime, envelope: Envelope) -> Result<()> {
    let inbox = runtime.state_root.join("control/inbox");
    fs::create_dir_all(&inbox)?;
    let target = inbox.join(format!("{}-{}.json", envelope.emitted_at_ms, envelope.id));
    let temporary = target.with_extension("tmp");
    fs::write(&temporary, envelope.to_ndjson()?)?;
    fs::rename(temporary, target)?;
    Ok(())
}

struct NetworkUploadSource {
    source_id: String,
    recording: Option<RecordingAvailable>,
}

impl SourceAdapter<RecordingAvailable> for NetworkUploadSource {
    type Error = anyhow::Error;

    fn adapter_id(&self) -> &str {
        "network-upload"
    }

    fn source_id(&self) -> &str {
        &self.source_id
    }

    fn capabilities(&self) -> &[SourceCapability] {
        &[SourceCapability::Sync, SourceCapability::BackgroundIngest]
    }

    fn execute(
        &mut self,
        command: SourceCommand,
    ) -> Result<Vec<SourceAdapterOutput<RecordingAvailable>>> {
        if command.adapter_id != self.adapter_id()
            || command
                .source_id
                .as_deref()
                .is_some_and(|source_id| source_id != self.source_id())
        {
            bail!("source command identity does not match network-upload adapter");
        }
        if !matches!(command.operation, SourceOperation::Sync { .. }) {
            bail!("network-upload adapter supports only sync");
        }
        let recording = self
            .recording
            .take()
            .context("network-upload adapter has no staged recording")?;
        Ok(vec![
            SourceAdapterOutput::Status(SourceStatus {
                adapter_id: self.adapter_id().into(),
                source_id: self.source_id.clone(),
                display_name: "Authenticated network upload".into(),
                state: SourceState::Ready,
                capabilities: self.capabilities().to_vec(),
                attributes: Default::default(),
            }),
            SourceAdapterOutput::Artifact(recording),
        ])
    }
}

fn ingest_audio_upload(runtime: &Runtime, request: &Request) -> Result<Response> {
    if request.body.is_empty() {
        return Ok(Response::json(
            400,
            json!({"error":"audio body is required"}),
        ));
    }
    let content_type = request
        .headers
        .get("content-type")
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .unwrap_or("");
    let Some(extension) = audio_extension(content_type) else {
        return Ok(Response::json(
            400,
            json!({"error":"unsupported audio content type"}),
        ));
    };
    let source_id = request
        .headers
        .get("x-source-id")
        .map(String::as_str)
        .unwrap_or("network-client")
        .trim();
    if source_id.is_empty() || source_id.len() > 128 || source_id.chars().any(char::is_control) {
        return Ok(Response::json(
            400,
            json!({"error":"invalid source identity"}),
        ));
    }
    let recorded_at_ms = request
        .headers
        .get("x-recorded-at-ms")
        .map(|value| value.parse::<i64>())
        .transpose();
    let recorded_at_ms = match recorded_at_ms {
        Ok(Some(value)) if value > 0 => value,
        Ok(None) => Utc::now().timestamp_millis(),
        _ => {
            return Ok(Response::json(
                400,
                json!({"error":"x-recorded-at-ms must be a positive integer"}),
            ));
        }
    };

    let upload_id = Uuid::new_v4();
    let directory = runtime.state_root.join("source-inbox/network-upload");
    fs::create_dir_all(&directory)?;
    let destination = directory.join(format!("{upload_id}.{extension}"));
    let temporary = destination.with_extension(format!("{extension}.partial"));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let persist = (|| -> Result<()> {
        let mut file = options.open(&temporary)?;
        file.write_all(&request.body)?;
        file.sync_all()?;
        fs::rename(&temporary, &destination)?;
        Ok(())
    })();
    if let Err(error) = persist {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }

    let dispatch = (|| -> Result<()> {
        let mut adapter = NetworkUploadSource {
            source_id: source_id.to_owned(),
            recording: Some(RecordingAvailable {
                source_id: source_id.to_owned(),
                adapter_id: "network-upload".into(),
                source_path: destination.to_string_lossy().into_owned(),
                recorded_at_ms,
            }),
        };
        let outputs = execute_recording_source(
            &mut adapter,
            SourceCommand {
                adapter_id: "network-upload".into(),
                source_id: Some(source_id.to_owned()),
                operation: SourceOperation::Sync {
                    destination_uri: "lake://recordings".into(),
                    idle_timeout_ms: 0,
                },
            },
        )
        .map_err(anyhow::Error::msg)?;
        let recording = outputs
            .into_iter()
            .find_map(|output| match output {
                SourceAdapterOutput::Artifact(recording) => Some(recording),
                SourceAdapterOutput::Status(_) => None,
            })
            .context("network-upload adapter returned no recording")?;
        enqueue(
            runtime,
            Envelope::root(Message::RecordingAvailable(recording)),
        )
    })();
    if let Err(error) = dispatch {
        let _ = fs::remove_file(&destination);
        return Err(error);
    }
    Ok(Response::json(
        202,
        json!({
            "status":"queued",
            "upload_id":upload_id,
            "source_id":source_id,
            "recorded_at_ms":recorded_at_ms
        }),
    ))
}

fn audio_extension(content_type: &str) -> Option<&'static str> {
    match content_type {
        "audio/mpeg" => Some("mp3"),
        "audio/mp4" | "audio/x-m4a" => Some("m4a"),
        "audio/wav" | "audio/x-wav" => Some("wav"),
        "audio/aac" => Some("aac"),
        "audio/flac" | "audio/x-flac" => Some("flac"),
        "audio/ogg" => Some("ogg"),
        "audio/opus" => Some("opus"),
        _ => None,
    }
}

fn clickhouse_json(runtime: &Runtime, query: &str) -> Result<Value> {
    let response = reqwest::blocking::Client::new()
        .post(&runtime.clickhouse_url)
        .body(query.to_owned())
        .send()?;
    let status = response.status();
    let bytes = response.bytes()?;
    if !status.is_success() {
        bail!("ClickHouse {status}: {}", String::from_utf8_lossy(&bytes));
    }
    serde_json::from_slice(&bytes).context("decode ClickHouse JSON response")
}

fn atomic_json(path: &Path, value: &Value) -> Result<()> {
    fs::create_dir_all(path.parent().context("JSON file has no parent")?)?;
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(value)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn ensure_token(state_root: &Path) -> Result<String> {
    let path = state_root.join("api-token");
    if path.exists() {
        return Ok(fs::read_to_string(path)?.trim().to_owned());
    }
    fs::create_dir_all(state_root)?;
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    writeln!(file, "{token}")?;
    Ok(token)
}

fn validate_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        bail!("id must contain only letters, numbers, '-' and '_'");
    }
    Ok(())
}

fn json_uuid(value: &Value, key: &str) -> Result<Uuid> {
    Uuid::parse_str(
        value
            .get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("{key} is required"))?,
    )
    .map_err(Into::into)
}

fn sql_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}

#[derive(Debug)]
struct Request {
    method: String,
    path: String,
    query: HashMap<String, String>,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

impl Request {
    fn read(stream: &mut TcpStream) -> Result<Self> {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 4096];
        let header_end = loop {
            let read = stream.read(&mut buffer)?;
            if read == 0 {
                bail!("connection closed before request headers");
            }
            bytes.extend_from_slice(&buffer[..read]);
            if bytes.len() > MAX_HEADER_BYTES {
                bail!("request headers are too large");
            }
            if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                break position + 4;
            }
        };
        let header_text = std::str::from_utf8(&bytes[..header_end])?.to_owned();
        let mut lines = header_text.split("\r\n");
        let first = lines.next().context("missing HTTP request line")?;
        let mut request_line = first.split_whitespace();
        let method = request_line.next().context("missing method")?.to_owned();
        let target = request_line.next().context("missing request target")?;
        let mut headers = HashMap::new();
        for line in lines.filter(|line| !line.is_empty()) {
            if let Some((name, value)) = line.split_once(':') {
                headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
            }
        }
        if headers.contains_key("transfer-encoding") {
            bail!("transfer-encoded requests are not supported");
        }
        let content_length = headers
            .get("content-length")
            .map(|value| value.parse::<usize>())
            .transpose()
            .context("invalid content-length")?
            .unwrap_or(0);
        let maximum_body =
            if method == "POST" && target.split('?').next() == Some("/api/v1/ingest/audio") {
                MAX_AUDIO_UPLOAD_BYTES
            } else {
                MAX_REQUEST_BYTES
            };
        if content_length > maximum_body {
            bail!("request body is too large");
        }
        while bytes.len() < header_end + content_length {
            let read = stream.read(&mut buffer)?;
            if read == 0 {
                bail!("connection closed during request body");
            }
            bytes.extend_from_slice(&buffer[..read]);
        }
        let (path, raw_query) = target.split_once('?').unwrap_or((target, ""));
        let query = raw_query
            .split('&')
            .filter(|part| !part.is_empty())
            .map(|part| {
                let (key, value) = part.split_once('=').unwrap_or((part, ""));
                (url_decode(key), url_decode(value))
            })
            .collect();
        Ok(Self {
            method,
            path: path.to_owned(),
            query,
            headers,
            body: bytes[header_end..header_end + content_length].to_vec(),
        })
    }

    fn authorized(&self, token: &str) -> bool {
        self.headers
            .get("authorization")
            .is_some_and(|value| value == &format!("Bearer {token}"))
            || self.headers.get("cookie").is_some_and(|cookies| {
                cookies.split(';').any(|cookie| {
                    cookie.trim().strip_prefix("personal_audio_lake_token=") == Some(token)
                })
            })
    }
}

struct Response {
    status: u16,
    content_type: &'static str,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Response {
    fn html(value: &str) -> Self {
        Self::bytes(200, "text/html; charset=utf-8", value.as_bytes().to_vec())
    }

    fn json(status: u16, value: Value) -> Self {
        Self::bytes(
            status,
            "application/json; charset=utf-8",
            serde_json::to_vec(&value).expect("JSON value serializes"),
        )
    }

    fn bytes(status: u16, content_type: &'static str, body: Vec<u8>) -> Self {
        Self {
            status,
            content_type,
            headers: Vec::new(),
            body,
        }
    }

    fn header(mut self, name: &str, value: String) -> Self {
        self.headers.push((name.to_owned(), value));
        self
    }

    fn write(self, stream: &mut TcpStream) -> Result<()> {
        let reason = match self.status {
            200 => "OK",
            202 => "Accepted",
            400 => "Bad Request",
            401 => "Unauthorized",
            413 => "Payload Too Large",
            404 => "Not Found",
            _ => "Internal Server Error",
        };
        write!(
            stream,
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'self'; style-src 'self' 'unsafe-inline'; script-src 'self' 'unsafe-inline'; media-src 'self'\r\nConnection: close\r\n",
            self.status,
            reason,
            self.content_type,
            self.body.len()
        )?;
        for (name, value) in self.headers {
            write!(stream, "{name}: {value}\r\n")?;
        }
        write!(stream, "\r\n")?;
        stream.write_all(&self.body)?;
        stream.flush()?;
        Ok(())
    }
}

fn url_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => output.push(b' '),
            b'%' if index + 2 < bytes.len() => {
                if let Ok(decoded) = u8::from_str_radix(&value[index + 1..index + 3], 16) {
                    output.push(decoded);
                    index += 2;
                }
            }
            byte => output.push(byte),
        }
        index += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_path_safe() {
        assert!(validate_id("calendar-export_1").is_ok());
        assert!(validate_id("../secrets").is_err());
    }

    #[test]
    fn upload_content_types_are_closed_and_audio_only() {
        assert_eq!(audio_extension("audio/mpeg"), Some("mp3"));
        assert_eq!(audio_extension("audio/x-m4a"), Some("m4a"));
        assert_eq!(audio_extension("application/octet-stream"), None);
        assert_eq!(audio_extension("text/plain"), None);
    }

    #[test]
    fn upload_uses_the_generic_recording_event_ingress() {
        let root =
            std::env::temp_dir().join(format!("personal-audio-lake-upload-{}", Uuid::new_v4()));
        let runtime = Runtime {
            bind: "127.0.0.1:0".into(),
            state_root: root.join("state"),
            lake_root: root.join("lake"),
            clickhouse_url: "http://127.0.0.1:1".into(),
        };
        let request = Request {
            method: "POST".into(),
            path: "/api/v1/ingest/audio".into(),
            query: HashMap::new(),
            headers: HashMap::from([
                ("content-type".into(), "audio/mpeg".into()),
                ("x-source-id".into(), "android-test".into()),
                ("x-recorded-at-ms".into(), "1700000000000".into()),
            ]),
            body: b"fixture audio".to_vec(),
        };
        let response = ingest_audio_upload(&runtime, &request).expect("upload accepted");
        assert_eq!(response.status, 202);
        let entry = fs::read_dir(runtime.state_root.join("control/inbox"))
            .expect("control inbox")
            .next()
            .expect("one event")
            .expect("event entry");
        let event = Envelope::from_json(&fs::read_to_string(entry.path()).expect("read event"))
            .expect("generic envelope");
        let Message::RecordingAvailable(recording) = event.message else {
            panic!("upload did not emit RecordingAvailable");
        };
        assert_eq!(recording.adapter_id, "network-upload");
        assert_eq!(recording.source_id, "android-test");
        personal_audio_lake_contracts::validate_source_contract(
            "network-upload",
            "android-test",
            &[personal_audio_lake_contracts::SourceAdapterOutput::Artifact(recording.clone())],
        )
        .expect("network upload satisfies source contract");
        assert_eq!(
            fs::read(recording.source_path).expect("staged bytes"),
            b"fixture audio"
        );
        fs::remove_dir_all(root).expect("remove test upload root");
    }

    #[test]
    fn external_effect_lifecycle_uses_generic_action_receipts() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime {
            bind: "127.0.0.1:0".into(),
            state_root: root.path().join("state"),
            lake_root: root.path().join("lake"),
            clickhouse_url: "http://127.0.0.1:1".into(),
        };
        let action_id = Uuid::new_v4();
        let delivery = Request {
            method: "POST".into(),
            path: format!("/api/v1/actions/{action_id}/delivery"),
            query: HashMap::new(),
            headers: HashMap::new(),
            body: serde_json::to_vec(&json!({
                "target":"fixture.calendar",
                "external_id":"event-7",
                "delivered_at_ms":100
            }))
            .unwrap(),
        };
        assert_eq!(dynamic_route(&delivery, &runtime).unwrap().status, 202);

        let undo = Request {
            method: "POST".into(),
            path: format!("/api/v1/actions/{action_id}/external-undo"),
            query: HashMap::new(),
            headers: HashMap::new(),
            body: serde_json::to_vec(&json!({
                "target":"fixture.calendar",
                "undone_at_ms":200
            }))
            .unwrap(),
        };
        assert_eq!(dynamic_route(&undo, &runtime).unwrap().status, 202);

        let messages = fs::read_dir(runtime.state_root.join("control/inbox"))
            .unwrap()
            .map(|entry| {
                Envelope::from_json(&fs::read_to_string(entry.unwrap().path()).unwrap()).unwrap()
            })
            .collect::<Vec<_>>();
        assert!(matches!(
            messages.iter().find_map(|envelope| match &envelope.message {
                Message::ActionDelivered(receipt) => Some(receipt),
                _ => None,
            }),
            Some(receipt) if receipt.action_id == action_id
                && receipt.target == "fixture.calendar"
                && receipt.external_id == "event-7"
        ));
        assert!(matches!(
            messages.iter().find_map(|envelope| match &envelope.message {
                Message::ActionUndone(receipt) => Some(receipt),
                _ => None,
            }),
            Some(receipt) if receipt.action_id == action_id && receipt.target == "fixture.calendar"
        ));
    }

    #[test]
    fn external_effect_receipts_reject_ambiguous_identifiers() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime {
            bind: "127.0.0.1:0".into(),
            state_root: root.path().join("state"),
            lake_root: root.path().join("lake"),
            clickhouse_url: "http://127.0.0.1:1".into(),
        };
        let action_id = Uuid::new_v4();
        let request = Request {
            method: "POST".into(),
            path: format!("/api/v1/actions/{action_id}/delivery"),
            query: HashMap::new(),
            headers: HashMap::new(),
            body: serde_json::to_vec(&json!({
                "target":"fixture\ncalendar",
                "external_id":"event",
                "delivered_at_ms":100
            }))
            .unwrap(),
        };
        assert!(dynamic_route(&request, &runtime).is_err());
    }

    #[test]
    fn default_rules_cover_all_three_modes() {
        let rules = default_rules();
        let encoded = rules.to_string();
        assert!(encoded.contains("explicit"));
        assert!(encoded.contains("hybrid"));
        assert!(encoded.contains("implicit"));
        assert!(!encoded.contains("always_review"));
        assert!(!encoded.contains("review_when_ambiguous"));
    }

    #[test]
    fn service_api_accepts_the_user_space_osm_contract() {
        let workflow = WorkflowDefinition {
            id: "notes".into(),
            name: "Notes".into(),
            description: "Keep selected notes".into(),
            enabled: true,
            subscriptions: vec!["transcript_completed".into()],
            initial_state_id: "message_ingested".into(),
            states: vec![personal_audio_lake_contracts::WorkflowState {
                id: "message_ingested".into(),
                description: "A message arrived".into(),
                selector: personal_audio_lake_contracts::WorkflowSelector::Terminal {
                    outcome: "complete".into(),
                },
                transitions: vec![],
            }],
        };
        assert!(validate_service(&serde_json::to_value(workflow).unwrap()).is_ok());
    }

    #[test]
    fn eventkit_permission_requests_are_queued_for_the_native_app_identity() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime {
            bind: "127.0.0.1:0".into(),
            state_root: root.path().join("state"),
            lake_root: root.path().join("lake"),
            clickhouse_url: "http://127.0.0.1:1".into(),
        };
        let response = request_macos_access(&runtime, "request-calendar-access").unwrap();
        assert_eq!(response.status, 202);
        assert!(
            runtime
                .state_root
                .join("ui-commands/request-calendar-access")
                .exists()
        );
    }

    #[test]
    fn health_reports_degraded_when_storage_is_unavailable() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime {
            bind: "127.0.0.1:0".into(),
            state_root: root.path().join("state"),
            lake_root: root.path().join("lake"),
            clickhouse_url: "http://127.0.0.1:1".into(),
        };
        let response = health(&runtime).unwrap();
        let body: Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(body["status"], "degraded");
        assert_eq!(body["storage"]["status"], "unavailable");
        assert!(
            body["storage"]["error"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );
    }

    #[test]
    fn health_requires_current_readiness_from_every_worker_role() {
        let roles = [
            personal_audio_lake_contracts::WorkerRole::Source,
            personal_audio_lake_contracts::WorkerRole::TransferIngress,
            personal_audio_lake_contracts::WorkerRole::Lake,
            personal_audio_lake_contracts::WorkerRole::Transcribe,
            personal_audio_lake_contracts::WorkerRole::Derive,
            personal_audio_lake_contracts::WorkerRole::Calendar,
            personal_audio_lake_contracts::WorkerRole::Notify,
            personal_audio_lake_contracts::WorkerRole::Services,
        ];
        let status = roles
            .iter()
            .map(|role| {
                let event = Envelope::root(Message::WorkerHealth(
                    personal_audio_lake_contracts::WorkerHealth {
                        role: *role,
                        state: personal_audio_lake_contracts::HealthState::Ready,
                        detail: "fixture ready".into(),
                        backlog: 0,
                    },
                ));
                serde_json::to_string(&json!({"worker":"fixture","event":event})).unwrap()
            })
            .collect::<Vec<_>>()
            .join("\n");
        let (workers, ready) = worker_readiness(&status);
        assert!(ready);
        assert_eq!(workers.len(), roles.len());

        let (_, incomplete) = worker_readiness(
            &status
                .lines()
                .take(roles.len() - 1)
                .collect::<Vec<_>>()
                .join("\n"),
        );
        assert!(!incomplete);
    }

    #[test]
    fn successful_replay_supersedes_an_earlier_failure() {
        let root = tempfile::tempdir().unwrap();
        let manifest = root.path().join("lake/manifests/events.ndjson");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let fixtures = personal_audio_lake_contracts::contract_fixtures();
        let transcript = fixtures
            .iter()
            .find(|envelope| matches!(envelope.message, Message::TranscriptCompleted(_)))
            .unwrap();
        let completed = fixtures
            .iter()
            .find_map(|envelope| match &envelope.message {
                Message::DerivationCompleted(value) => Some(value.clone()),
                _ => None,
            })
            .unwrap();
        let failure = Envelope::caused_by(
            transcript,
            Message::Failure(personal_audio_lake_contracts::Failure {
                worker: personal_audio_lake_contracts::WorkerRole::Derive,
                code: "local_24b_judge_failed".into(),
                detail: "old model failure".into(),
                retryable: true,
                failed_message_id: Some(transcript.id),
            }),
        );
        fs::write(
            &manifest,
            format!(
                "{}{}",
                transcript.to_ndjson().unwrap(),
                failure.to_ndjson().unwrap()
            ),
        )
        .unwrap();
        let runtime = Runtime {
            bind: "127.0.0.1:0".into(),
            state_root: root.path().join("state"),
            lake_root: root.path().join("lake"),
            clickhouse_url: "http://127.0.0.1:1".into(),
        };
        assert_eq!(manifest_failures(&runtime).unwrap().len(), 1);

        let success = Envelope::caused_by(transcript, Message::DerivationCompleted(completed));
        let mut file = OpenOptions::new().append(true).open(&manifest).unwrap();
        file.write_all(success.to_ndjson().unwrap().as_bytes())
            .unwrap();
        assert!(manifest_failures(&runtime).unwrap().is_empty());
    }

    #[test]
    fn an_action_free_derivation_is_not_a_failure() {
        let root = tempfile::tempdir().unwrap();
        let manifest = root.path().join("lake/manifests/events.ndjson");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        let transcript = personal_audio_lake_contracts::contract_fixtures()
            .into_iter()
            .find(|envelope| matches!(envelope.message, Message::TranscriptCompleted(_)))
            .unwrap();
        let historical_noise = Envelope::caused_by(
            &transcript,
            Message::Failure(personal_audio_lake_contracts::Failure {
                worker: personal_audio_lake_contracts::WorkerRole::Derive,
                code: "no_action_candidates".into(),
                detail: "no action was requested".into(),
                retryable: false,
                failed_message_id: Some(transcript.id),
            }),
        );
        fs::write(&manifest, historical_noise.to_ndjson().unwrap()).unwrap();
        let runtime = Runtime {
            bind: "127.0.0.1:0".into(),
            state_root: root.path().join("state"),
            lake_root: root.path().join("lake"),
            clickhouse_url: "http://127.0.0.1:1".into(),
        };
        assert!(manifest_failures(&runtime).unwrap().is_empty());
    }

    #[test]
    fn action_projection_tracks_undo_and_later_redelivery() {
        let action_id = Uuid::new_v4();
        let delivery = Envelope::root(Message::ActionDelivered(
            personal_audio_lake_contracts::ActionDelivery {
                action_id,
                external_id: "first".into(),
                delivered_at_ms: 1,
                target: "macos_reminders".into(),
            },
        ));
        let undo = Envelope::root(Message::ActionUndone(
            personal_audio_lake_contracts::ActionUndo {
                action_id,
                target: "macos_reminders".into(),
                undone_at_ms: 2,
            },
        ));
        let redelivery = Envelope::root(Message::ActionDelivered(
            personal_audio_lake_contracts::ActionDelivery {
                action_id,
                external_id: "second".into(),
                delivered_at_ms: 3,
                target: "macos_reminders".into(),
            },
        ));

        let undone = currently_undone_actions(std::io::Cursor::new(format!(
            "{}{}",
            delivery.to_ndjson().unwrap(),
            undo.to_ndjson().unwrap()
        )))
        .unwrap();
        assert!(undone.contains(&action_id));

        let active = currently_undone_actions(std::io::Cursor::new(format!(
            "{}{}{}",
            delivery.to_ndjson().unwrap(),
            undo.to_ndjson().unwrap(),
            redelivery.to_ndjson().unwrap()
        )))
        .unwrap();
        assert!(!active.contains(&action_id));
    }
}
