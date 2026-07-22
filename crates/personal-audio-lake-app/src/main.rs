use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use chrono_tz::Tz;
use clap::Parser;
use personal_audio_lake_contracts::{
    Envelope, HealthState, Message, RecordingAvailable, SourceAdapter, SourceAdapterOutput,
    SourceCapability, SourceCommand, SourceOperation, SourceState, SourceStatus, WorkerHealth,
    WorkerRole, execute_recording_source, ports::MessageTransport,
};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

mod api;
mod app_controller;
mod menu_bar;
mod read_model;
mod source_catalog;
mod text_ui;
mod text_view;
mod view_model;

#[derive(Parser)]
struct Cli {
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long)]
    once: bool,
    /// Run the persistent server. Without this flag, open the installed frontend.
    #[arg(long)]
    server: bool,
    /// Use the deterministic reference text view instead of AppKit.
    #[arg(long)]
    text_ui: bool,
    /// Apply one text-view command and exit. Repeat to exercise a navigation sequence.
    #[arg(long = "text-command", requires = "text_ui")]
    text_commands: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct Config {
    lake_root: PathBuf,
    state_root: PathBuf,
    clickhouse_url: String,
    api_bind: String,
    manifest_only: bool,
    source_adapter_id: String,
    source_adapter_binary: String,
    #[serde(alias = "approved_device_id")]
    selected_source_id: Option<String>,
    #[serde(alias = "allow_unbound_pairing")]
    allow_source_pairing: bool,
    #[serde(skip)]
    release_in_progress: bool,
    scan_interval_seconds: u64,
    scan_duration_ms: u64,
    timezone: String,
    watched_folders: Vec<PathBuf>,
    usb_folders: Vec<PathBuf>,
    #[serde(default = "default_idle_timeout")]
    source_idle_timeout_ms: u64,
    #[serde(default = "default_transfer_interval")]
    source_sync_interval_seconds: u64,
}

fn default_idle_timeout() -> u64 {
    3_000
}
fn default_transfer_interval() -> u64 {
    60
}

fn system_timezone() -> String {
    let valid = |value: String| value.parse::<Tz>().ok().map(|_| value);
    if let Some(value) = std::env::var("TZ").ok().and_then(valid) {
        return value;
    }
    if let Ok(target) = fs::read_link("/etc/localtime") {
        let target = target.to_string_lossy();
        if let Some((_, value)) = target.split_once("zoneinfo/")
            && let Some(value) = valid(value.to_owned())
        {
            return value;
        }
    }
    "Etc/UTC".into()
}

impl Default for Config {
    fn default() -> Self {
        let support = home().join("Library/Application Support/PersonalAudioLake");
        Self {
            lake_root: support.join("lake"),
            state_root: support,
            clickhouse_url: "http://127.0.0.1:8123".into(),
            api_bind: "127.0.0.1:32145".into(),
            manifest_only: false,
            source_adapter_id: String::new(),
            source_adapter_binary: String::new(),
            selected_source_id: None,
            allow_source_pairing: false,
            release_in_progress: false,
            scan_interval_seconds: 30,
            scan_duration_ms: 8_000,
            timezone: system_timezone(),
            watched_folders: vec![],
            usb_folders: vec![],
            source_idle_timeout_ms: 8_000,
            source_sync_interval_seconds: 60,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Role {
    Source,
    TransferIngress,
    Lake,
    Transcribe,
    Derive,
    Calendar,
    Notify,
    Services,
}

struct Worker {
    child: Child,
    transport: NdjsonTransport<ChildStdin>,
}

struct NdjsonTransport<Writer> {
    writer: Writer,
}

impl<Writer: Write> MessageTransport<Envelope> for NdjsonTransport<Writer> {
    type Error = anyhow::Error;

    fn send(&mut self, envelope: &Envelope) -> Result<()> {
        self.writer.write_all(envelope.to_ndjson()?.as_bytes())?;
        self.writer.flush()?;
        Ok(())
    }
}

impl Worker {
    fn send(&mut self, envelope: &Envelope) -> Result<()> {
        self.transport.send(envelope)
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let config_path = cli.config.unwrap_or_else(default_config_path);
    let config = load_or_create_config(&config_path)?;
    if cli.text_ui {
        return text_ui::run(&config, &cli.text_commands);
    }
    if !cli.server && !cli.once {
        let commands = config.state_root.join("ui-commands");
        fs::create_dir_all(&commands)?;
        fs::write(
            commands.join("open-lake"),
            Utc::now().timestamp_millis().to_string(),
        )?;
        return Ok(());
    }
    if cli.server && !cli.once {
        let worker_config = config.clone();
        let worker_config_path = config_path.clone();
        let state_root = config.state_root.clone();
        let (arrival_tx, arrival_rx) = mpsc::channel();
        let unseen = menu_bar::unseen_arrivals(&state_root);
        if !unseen.is_empty() {
            let _ = arrival_tx.send(menu_bar::AppEvent::CatchUp(unseen));
        }
        let snapshot_tx = arrival_tx.clone();
        let snapshot_bind = config.api_bind.clone();
        let snapshot_state = state_root.clone();
        thread::Builder::new()
            .name("personal-audio-lake-read-model".into())
            .spawn(move || {
                loop {
                    if read_model::fetch(&snapshot_bind, &snapshot_state).is_ok()
                        && snapshot_tx.send(menu_bar::AppEvent::ModelChanged).is_err()
                    {
                        break;
                    }
                    thread::sleep(Duration::from_secs(3));
                }
            })?;
        thread::Builder::new()
            .name("personal-audio-lake-server".into())
            .spawn(move || {
                if let Err(error) =
                    run_pipeline(worker_config, worker_config_path, false, Some(arrival_tx))
                {
                    eprintln!("local server stopped: {error:#}");
                    std::process::exit(1);
                }
            })?;
        return menu_bar::run(&config, arrival_rx);
    }
    run_pipeline(config, config_path, cli.once, None)
}

fn run_pipeline(
    mut config: Config,
    config_path: PathBuf,
    once: bool,
    arrival_tx: Option<Sender<menu_bar::AppEvent>>,
) -> Result<()> {
    prepare_state(&config)?;
    api::start(api::Runtime {
        bind: config.api_bind.clone(),
        state_root: config.state_root.clone(),
        lake_root: config.lake_root.clone(),
        clickhouse_url: config.clickhouse_url.clone(),
    })?;
    let (tx, rx) = mpsc::channel();
    let mut workers = spawn_workers(&config, tx.clone())?;
    if !device_source_enabled(&config) {
        route(
            Role::Source,
            &Envelope::root(Message::WorkerHealth(WorkerHealth {
                role: WorkerRole::Source,
                state: HealthState::Ready,
                detail:
                    "Optional source adapter disabled; file, folder, and network ingestion ready"
                        .into(),
                backlog: 0,
            })),
            &config,
            &mut workers,
        )?;
    }
    let once_started = Instant::now();
    let mut last_activity = Instant::now();
    let mut last_scan = Instant::now() - Duration::from_secs(config.scan_interval_seconds);
    let mut last_transfer =
        Instant::now() - Duration::from_secs(config.source_sync_interval_seconds);
    let mut projected_source_states = HashMap::new();

    loop {
        ensure_workers_alive(&mut workers)?;
        if last_scan.elapsed() >= Duration::from_secs(config.scan_interval_seconds)
            && device_source_enabled(&config)
            && !config.release_in_progress
            && (config.selected_source_id.is_some() || config.allow_source_pairing)
        {
            workers
                .get_mut(&Role::Source)
                .unwrap()
                .send(&Envelope::root(Message::SourceCommandRequested(
                    SourceCommand {
                        adapter_id: config.source_adapter_id.clone(),
                        source_id: config.selected_source_id.clone(),
                        operation: SourceOperation::Discover {
                            duration_ms: config.scan_duration_ms,
                            allow_pairing: config.allow_source_pairing,
                        },
                    },
                )))?;
            last_scan = Instant::now();
        }
        ingest_folder_candidates(&config, workers.get_mut(&Role::Lake).unwrap())?;
        ingest_control_messages(&mut config, &config_path, &tx, &mut workers)?;

        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok((role, envelope)) => {
                last_activity = Instant::now();
                if should_start_transfer_ingress(
                    role,
                    &envelope,
                    workers.contains_key(&Role::TransferIngress),
                ) {
                    workers.insert(
                        Role::TransferIngress,
                        spawn_worker(
                            &config,
                            &tx,
                            Role::TransferIngress,
                            "personal-audio-lake-bluetooth",
                            vec![],
                        )?,
                    );
                }
                let mut project_source_status = true;
                let mut source_already_bound = false;
                if let Message::SourceStatusChanged(status) = &envelope.message
                    && status.state == SourceState::Ready
                    && !config.release_in_progress
                    && approved_for_transfer(&config, &status.source_id)
                    && last_transfer.elapsed()
                        >= Duration::from_secs(config.source_sync_interval_seconds)
                {
                    workers
                        .get_mut(&Role::Source)
                        .context("source status arrived without a configured device adapter")?
                        .send(&Envelope::caused_by(
                            &envelope,
                            Message::SourceCommandRequested(SourceCommand {
                                adapter_id: status.adapter_id.clone(),
                                source_id: Some(status.source_id.clone()),
                                operation: SourceOperation::Sync {
                                    destination_uri: format!(
                                        "file://{}",
                                        config.state_root.join("source-inbox").display()
                                    ),
                                    idle_timeout_ms: config.source_idle_timeout_ms,
                                },
                            }),
                        ))?;
                    last_transfer = Instant::now();
                }
                if let Message::SourceStatusChanged(status) = &envelope.message
                    && status.state == SourceState::Bound
                {
                    let changed = config.selected_source_id.as_deref() != Some(&status.source_id)
                        || config.allow_source_pairing;
                    config.selected_source_id = Some(status.source_id.clone());
                    config.allow_source_pairing = false;
                    if changed {
                        write_config(&config_path, &config)?;
                    } else {
                        source_already_bound = true;
                    }
                }
                if let Message::SourceStatusChanged(status) = &envelope.message
                    && status.state == SourceState::Released
                    && config.selected_source_id.as_deref() == Some(&status.source_id)
                {
                    config.selected_source_id = None;
                    config.allow_source_pairing = false;
                    config.release_in_progress = false;
                    write_config(&config_path, &config)?;
                }
                if let Message::SourceStatusChanged(status) = &envelope.message {
                    project_source_status = should_project_source_status(
                        &mut projected_source_states,
                        status,
                        source_already_bound,
                    );
                }
                if role == Role::Source
                    && config.release_in_progress
                    && matches!(&envelope.message, Message::Failure(_))
                {
                    config.release_in_progress = false;
                }
                if !matches!(&envelope.message, Message::SourceStatusChanged(_))
                    || project_source_status
                {
                    route(role, &envelope, &config, &mut workers)?;
                }
                if let Message::NotificationDelivered(delivery) = &envelope.message
                    && let Some(tx) = &arrival_tx
                {
                    let _ = tx.send(menu_bar::AppEvent::Live(menu_bar::ArrivalNotice::from(
                        delivery,
                    )));
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(error) => return Err(error.into()),
        }
        if once
            && once_started.elapsed() >= Duration::from_millis(config.scan_duration_ms + 500)
            && last_activity.elapsed() >= Duration::from_millis(500)
        {
            break;
        }
    }
    Ok(())
}

fn should_project_source_status(
    states: &mut HashMap<String, SourceState>,
    status: &personal_audio_lake_contracts::SourceStatus,
    source_already_bound: bool,
) -> bool {
    if status.state == SourceState::Bound && source_already_bound {
        return false;
    }
    if states.get(&status.source_id) == Some(&status.state) {
        return false;
    }
    states.insert(status.source_id.clone(), status.state);
    true
}

fn ensure_workers_alive(workers: &mut HashMap<Role, Worker>) -> Result<()> {
    for (role, worker) in workers {
        if let Some(status) = worker.child.try_wait()? {
            bail!("{role:?} worker exited with {status}; restart the server tree");
        }
    }
    Ok(())
}

fn route(
    role: Role,
    envelope: &Envelope,
    config: &Config,
    workers: &mut HashMap<Role, Worker>,
) -> Result<()> {
    match &envelope.message {
        Message::RecordingAvailable(_) => workers.get_mut(&Role::Lake).unwrap().send(envelope)?,
        Message::AudioIngested(_) => {
            workers.get_mut(&Role::Lake).unwrap().send(envelope)?;
            workers.get_mut(&Role::Transcribe).unwrap().send(envelope)?;
        }
        Message::TranscriptCompleted(_) => {
            workers.get_mut(&Role::Lake).unwrap().send(envelope)?;
            workers.get_mut(&Role::Derive).unwrap().send(envelope)?;
            workers.get_mut(&Role::Notify).unwrap().send(envelope)?;
        }
        Message::ActionProposed(_) => {
            workers.get_mut(&Role::Lake).unwrap().send(envelope)?;
            workers.get_mut(&Role::Calendar).unwrap().send(envelope)?;
            workers.get_mut(&Role::Notify).unwrap().send(envelope)?;
        }
        Message::ActionRevisionRequested(_) => {
            workers.get_mut(&Role::Lake).unwrap().send(envelope)?;
            workers.get_mut(&Role::Calendar).unwrap().send(envelope)?;
        }
        Message::ActionApproved { .. } => {
            workers.get_mut(&Role::Lake).unwrap().send(envelope)?;
            workers.get_mut(&Role::Calendar).unwrap().send(envelope)?;
            workers.get_mut(&Role::Notify).unwrap().send(envelope)?;
        }
        Message::ActionRejected { .. }
        | Message::TranscriptAnnotated(_)
        | Message::RecordingReviewed(_) => {
            workers.get_mut(&Role::Lake).unwrap().send(envelope)?;
        }
        Message::ActionDelivered(_) => {
            workers.get_mut(&Role::Lake).unwrap().send(envelope)?;
            workers.get_mut(&Role::Notify).unwrap().send(envelope)?;
        }
        Message::ActionUndone(_) => {
            workers.get_mut(&Role::Lake).unwrap().send(envelope)?;
            workers.get_mut(&Role::Services).unwrap().send(envelope)?;
        }
        Message::ActionUndoRequested { .. } => {
            workers.get_mut(&Role::Lake).unwrap().send(envelope)?;
            workers.get_mut(&Role::Calendar).unwrap().send(envelope)?;
        }
        Message::DerivationCompleted(_) => {
            workers.get_mut(&Role::Lake).unwrap().send(envelope)?;
            workers.get_mut(&Role::Notify).unwrap().send(envelope)?;
        }
        Message::NotificationDelivered(_) => {
            workers.get_mut(&Role::Lake).unwrap().send(envelope)?;
        }
        Message::WorkflowRunRecorded(_) => {
            workers.get_mut(&Role::Lake).unwrap().send(envelope)?;
            workers.get_mut(&Role::Notify).unwrap().send(envelope)?;
        }
        Message::WorkflowEffectUndone(_) | Message::ServiceDelivered(_) => {
            workers.get_mut(&Role::Lake).unwrap().send(envelope)?
        }
        message if forward_operational_fact_to_lake(role, message) => {
            workers.get_mut(&Role::Lake).unwrap().send(envelope)?;
        }
        _ => {}
    }
    append_status(config, role, envelope)?;
    Ok(())
}

fn forward_operational_fact_to_lake(role: Role, message: &Message) -> bool {
    role != Role::Lake
        && matches!(
            message,
            Message::SourceStatusChanged(_) | Message::Failure(_)
        )
}

fn spawn_workers(config: &Config, tx: Sender<(Role, Envelope)>) -> Result<HashMap<Role, Worker>> {
    let mut specs = Vec::new();
    if device_source_enabled(config) {
        specs.push((
            Role::Source,
            config.source_adapter_binary.as_str(),
            vec!["worker".into()],
        ));
    } else {
        specs.push((
            Role::TransferIngress,
            "personal-audio-lake-bluetooth",
            vec![],
        ));
    }
    specs.extend([
        (Role::Lake, "personal-audio-lake-storage", {
            let mut args = vec![
                "--lake-root".into(),
                config.lake_root.display().to_string(),
                "--clickhouse-url".into(),
                config.clickhouse_url.clone(),
            ];
            if config.manifest_only {
                args.push("--manifest-only".into());
            }
            args.push("worker".into());
            args
        }),
        (
            Role::Transcribe,
            "personal-audio-lake-transcription",
            vec![],
        ),
        (Role::Derive, "personal-audio-lake-workflows", vec![]),
        (
            Role::Notify,
            "personal-audio-lake-macos-notifications",
            vec![
                "--state-root".into(),
                config.state_root.display().to_string(),
            ],
        ),
        (
            Role::Calendar,
            "personal-audio-lake-macos-actions",
            vec![
                "--state-root".into(),
                config.state_root.display().to_string(),
                "worker".into(),
            ],
        ),
        (
            Role::Services,
            "personal-audio-lake-services",
            vec![
                "--state-root".into(),
                config.state_root.display().to_string(),
                "--lake-root".into(),
                config.lake_root.display().to_string(),
            ],
        ),
    ]);
    let mut workers = HashMap::new();
    for (role, binary, args) in specs {
        workers.insert(role, spawn_worker(config, &tx, role, binary, args)?);
    }
    Ok(workers)
}

fn device_source_enabled(config: &Config) -> bool {
    !config.source_adapter_id.trim().is_empty() && !config.source_adapter_binary.trim().is_empty()
}

fn should_start_transfer_ingress(
    producer: Role,
    envelope: &Envelope,
    already_started: bool,
) -> bool {
    !already_started
        && producer == Role::Source
        && matches!(
            &envelope.message,
            Message::WorkerHealth(WorkerHealth {
                role: WorkerRole::Source,
                state: HealthState::Ready,
                ..
            })
        )
}

fn spawn_worker(
    config: &Config,
    tx: &Sender<(Role, Envelope)>,
    role: Role,
    binary: &str,
    args: Vec<String>,
) -> Result<Worker> {
    let exe_dir = std::env::current_exe()?
        .parent()
        .ok_or_else(|| anyhow!("supervisor has no executable directory"))?
        .to_owned();
    let path = exe_dir.join(binary);
    let mut child = Command::new(&path)
        .args(args)
        .env("PERSONAL_AUDIO_LAKE_ROOT", &config.lake_root)
        .env("PERSONAL_AUDIO_LAKE_STATE_ROOT", &config.state_root)
        .env("PERSONAL_AUDIO_LAKE_TIMEZONE", &config.timezone)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("start {}", path.display()))?;
    let stdin = child.stdin.take().context("worker stdin")?;
    let stdout = child.stdout.take().context("worker stdout")?;
    let worker_tx = tx.clone();
    thread::Builder::new()
        .name(format!("{role:?}-messages"))
        .spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line.and_then(|line| {
                    Envelope::from_json(&line)
                        .map_err(|error| std::io::Error::other(error.to_string()))
                }) {
                    Ok(envelope) => {
                        if worker_tx.send((role, envelope)).is_err() {
                            break;
                        }
                    }
                    Err(error) => eprintln!("{role:?} emitted invalid message: {error}"),
                }
            }
        })?;
    Ok(Worker {
        child,
        transport: NdjsonTransport { writer: stdin },
    })
}

struct FolderSourceAdapter<'a> {
    root: &'a Path,
    adapter_id: &'a str,
    source_id: String,
    seen: &'a str,
}

impl<'a> FolderSourceAdapter<'a> {
    fn new(root: &'a Path, adapter_id: &'a str, seen: &'a str) -> Self {
        Self {
            root,
            adapter_id,
            source_id: root.display().to_string(),
            seen,
        }
    }
}

impl SourceAdapter<RecordingAvailable> for FolderSourceAdapter<'_> {
    type Error = anyhow::Error;

    fn adapter_id(&self) -> &str {
        self.adapter_id
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
        if command.adapter_id != self.adapter_id
            || command
                .source_id
                .as_deref()
                .is_some_and(|source_id| source_id != self.source_id)
        {
            bail!("source command identity does not match folder adapter");
        }
        if !matches!(command.operation, SourceOperation::Sync { .. }) {
            bail!("folder adapter supports only sync");
        }
        let mut outputs = vec![SourceAdapterOutput::Status(SourceStatus {
            adapter_id: self.adapter_id.to_owned(),
            source_id: self.source_id.clone(),
            display_name: self.root.display().to_string(),
            state: SourceState::Ready,
            capabilities: self.capabilities().to_vec(),
            attributes: Default::default(),
        })];
        for entry in WalkDir::new(self.root)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
        {
            let path = entry.path();
            if !entry.file_type().is_file() || !is_audio(path) {
                continue;
            }
            let metadata = entry.metadata()?;
            let modified = system_time_ms(metadata.modified().unwrap_or(UNIX_EPOCH));
            let key = recording_seen_key(path, metadata.len(), modified);
            if self.seen.lines().any(|line| line == key) {
                continue;
            }
            outputs.push(SourceAdapterOutput::Artifact(folder_recording(
                self.root,
                self.adapter_id,
                path,
                modified,
            )));
        }
        Ok(outputs)
    }
}

fn recording_seen_key(path: &Path, byte_len: u64, modified_at_ms: i64) -> String {
    format!("{}:{byte_len}:{modified_at_ms}", path.display())
}

fn ingest_folder_candidates(config: &Config, lake: &mut Worker) -> Result<()> {
    let seen_path = config.state_root.join("seen-recordings.ndjson");
    let mut seen = fs::read_to_string(&seen_path).unwrap_or_default();
    for (root, adapter_id) in config
        .usb_folders
        .iter()
        .map(|path| (path, "mounted-volume"))
        .chain(
            config
                .watched_folders
                .iter()
                .map(|path| (path, "watched-folder")),
        )
    {
        if !root.exists() {
            continue;
        }
        let source_id = root.display().to_string();
        let mut adapter = FolderSourceAdapter::new(root, adapter_id, &seen);
        let outputs = execute_recording_source(
            &mut adapter,
            SourceCommand {
                adapter_id: adapter_id.into(),
                source_id: Some(source_id),
                operation: SourceOperation::Sync {
                    destination_uri: "lake://recordings".into(),
                    idle_timeout_ms: 0,
                },
            },
        )
        .map_err(anyhow::Error::msg)?;
        for recording in outputs.into_iter().filter_map(|output| match output {
            SourceAdapterOutput::Artifact(recording) => Some(recording),
            SourceAdapterOutput::Status(_) => None,
        }) {
            lake.send(&Envelope::root(Message::RecordingAvailable(
                recording.clone(),
            )))?;
            let path = Path::new(&recording.source_path);
            let metadata = fs::metadata(path)?;
            let modified = system_time_ms(metadata.modified().unwrap_or(UNIX_EPOCH));
            let key = recording_seen_key(path, metadata.len(), modified);
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&seen_path)?;
            writeln!(file, "{key}")?;
            file.sync_data()?;
            seen.push_str(&key);
            seen.push('\n');
        }
    }
    Ok(())
}

fn folder_recording(
    root: &Path,
    adapter_id: &str,
    path: &Path,
    recorded_at_ms: i64,
) -> RecordingAvailable {
    RecordingAvailable {
        source_id: root.display().to_string(),
        adapter_id: adapter_id.into(),
        source_path: path.display().to_string(),
        recorded_at_ms,
    }
}

fn ingest_control_messages(
    config: &mut Config,
    config_path: &Path,
    tx: &Sender<(Role, Envelope)>,
    workers: &mut HashMap<Role, Worker>,
) -> Result<()> {
    let inbox = config.state_root.join("control/inbox");
    let processed = config.state_root.join("control/processed");
    let mut entries = fs::read_dir(&inbox)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        if !entry.file_type()?.is_file() {
            continue;
        }
        let envelope = Envelope::from_json(&fs::read_to_string(entry.path())?)?;
        match envelope.message {
            Message::ActionApproved { .. } => route(Role::Calendar, &envelope, config, workers)?,
            Message::ActionProposed(_) => route(Role::Calendar, &envelope, config, workers)?,
            Message::ActionRevisionRequested(_) => {
                workers.get_mut(&Role::Calendar).unwrap().send(&envelope)?;
                workers.get_mut(&Role::Lake).unwrap().send(&envelope)?;
            }
            Message::ActionUndoRequested { .. } => {
                workers.get_mut(&Role::Calendar).unwrap().send(&envelope)?;
                workers.get_mut(&Role::Lake).unwrap().send(&envelope)?;
            }
            Message::ActionRejected { .. }
            | Message::TranscriptAnnotated(_)
            | Message::RecordingReviewed(_) => route(Role::Lake, &envelope, config, workers)?,
            Message::SourceAdapterSelectionRequested { ref adapter_id } => {
                let descriptor = source_catalog::find(adapter_id)
                    .ok_or_else(|| anyhow!("unknown recording source adapter: {adapter_id}"))?;
                let changed = config.source_adapter_id != descriptor.id
                    || config.source_adapter_binary != descriptor.executable;
                if changed {
                    for role in [Role::Source, Role::TransferIngress] {
                        if let Some(mut worker) = workers.remove(&role) {
                            let _ = worker.child.kill();
                            let _ = worker.child.wait();
                        }
                    }
                    config.source_adapter_id = descriptor.id.into();
                    config.source_adapter_binary = descriptor.executable.into();
                    config.selected_source_id = None;
                    config.allow_source_pairing = true;
                    config.release_in_progress = false;
                    write_config(config_path, config)?;
                }
                if let std::collections::hash_map::Entry::Vacant(entry) =
                    workers.entry(Role::Source)
                {
                    entry.insert(spawn_worker(
                        config,
                        tx,
                        Role::Source,
                        descriptor.executable,
                        vec!["worker".into()],
                    )?);
                }
                workers
                    .get_mut(&Role::Source)
                    .unwrap()
                    .send(&Envelope::caused_by(
                        &envelope,
                        Message::SourceCommandRequested(SourceCommand {
                            adapter_id: descriptor.id.into(),
                            source_id: config.selected_source_id.clone(),
                            operation: SourceOperation::Discover {
                                duration_ms: config.scan_duration_ms,
                                allow_pairing: true,
                            },
                        }),
                    ))?;
            }
            Message::SourceCommandRequested(_) => {
                if !device_source_enabled(config) {
                    bail!("no device source adapter is configured");
                }
                config.release_in_progress = false;
                if config.selected_source_id.is_none() && !config.allow_source_pairing {
                    config.allow_source_pairing = true;
                    write_config(config_path, config)?;
                }
                workers
                    .get_mut(&Role::Source)
                    .unwrap()
                    .send(&Envelope::caused_by(
                        &envelope,
                        Message::SourceCommandRequested(SourceCommand {
                            adapter_id: config.source_adapter_id.clone(),
                            source_id: config.selected_source_id.clone(),
                            operation: SourceOperation::Discover {
                                duration_ms: config.scan_duration_ms,
                                allow_pairing: true,
                            },
                        }),
                    ))?;
            }
            Message::ReleaseSourceRequested => {
                if !device_source_enabled(config) {
                    bail!("no device source adapter is configured");
                }
                let source_id = config
                    .selected_source_id
                    .clone()
                    .ok_or_else(|| anyhow!("no source is currently paired"))?;
                let request = Envelope::caused_by(
                    &envelope,
                    Message::SourceCommandRequested(SourceCommand {
                        adapter_id: config.source_adapter_id.clone(),
                        source_id: Some(source_id),
                        operation: SourceOperation::Release,
                    }),
                );
                config.release_in_progress = true;
                if let Some(mut worker) = workers.remove(&Role::Source) {
                    let _ = worker.child.kill();
                    let _ = worker.child.wait();
                }
                workers.insert(
                    Role::Source,
                    spawn_worker(
                        config,
                        tx,
                        Role::Source,
                        &config.source_adapter_binary,
                        vec!["worker".into()],
                    )?,
                );
                workers.get_mut(&Role::Source).unwrap().send(&request)?;
            }
            Message::RecordingAvailable(_) => {
                workers.get_mut(&Role::Lake).unwrap().send(&envelope)?
            }
            Message::TranscriptCompleted(_) => route(Role::Transcribe, &envelope, config, workers)?,
            Message::WorkflowRunRequested(_)
            | Message::WorkflowRoutingRequested(_)
            | Message::WorkflowTransitionAdjusted(_)
            | Message::WorkflowEffectUndoRequested(_) => {
                workers.get_mut(&Role::Services).unwrap().send(&envelope)?;
                workers.get_mut(&Role::Lake).unwrap().send(&envelope)?;
            }
            Message::WorkflowRunConfirmed(_) => {
                workers.get_mut(&Role::Lake).unwrap().send(&envelope)?;
            }
            _ => bail!("unsupported control message in {}", entry.path().display()),
        }
        fs::rename(entry.path(), processed.join(entry.file_name()))?;
    }
    Ok(())
}

fn prepare_state(config: &Config) -> Result<()> {
    for path in [
        config.state_root.clone(),
        config.lake_root.clone(),
        config.state_root.join("source-inbox"),
        config.state_root.join("control/inbox"),
        config.state_root.join("control/processed"),
        config.state_root.join("status"),
    ] {
        fs::create_dir_all(path)?;
    }
    // Worker readiness is a property of this process generation. Keeping stale Ready events from
    // a previous run would let setup report success before the replacement workers have started.
    fs::write(config.state_root.join("status/events.ndjson"), b"")?;
    Ok(())
}

fn load_or_create_config(path: &Path) -> Result<Config> {
    let config = if !path.exists() {
        let config = Config::default();
        fs::create_dir_all(path.parent().context("config has no parent")?)?;
        config
    } else {
        toml::from_str(&fs::read_to_string(path)?).context("parse Captain’s Log config")?
    };
    write_config(path, &config)?;
    Ok(config)
}

fn write_config(path: &Path, config: &Config) -> Result<()> {
    let temporary = path.with_extension("toml.tmp");
    fs::write(&temporary, toml::to_string_pretty(config)?)?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn append_status(config: &Config, role: Role, envelope: &Envelope) -> Result<()> {
    let path = config.state_root.join("status/events.ndjson");
    compact_status_tail(&path, 16 * 1024 * 1024, 8 * 1024 * 1024)?;
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(
        file,
        "{}",
        serde_json::to_string(&serde_json::json!({
            "worker": format!("{role:?}").to_lowercase(),
            "event": envelope,
        }))?
    )?;
    Ok(())
}

fn compact_status_tail(path: &Path, maximum_bytes: u64, retained_bytes: u64) -> Result<()> {
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let length = file.metadata()?.len();
    if length <= maximum_bytes {
        return Ok(());
    }
    file.seek(SeekFrom::Start(length.saturating_sub(retained_bytes)))?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail)?;
    if let Some(first_newline) = tail.iter().position(|byte| *byte == b'\n') {
        tail.drain(..=first_newline);
    }
    let temporary = path.with_extension("ndjson.compacting");
    fs::write(&temporary, tail)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn approved_for_transfer(config: &Config, device_id: &str) -> bool {
    config
        .selected_source_id
        .as_deref()
        .is_none_or(|approved| approved == device_id)
}

fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "wav" | "m4a" | "mp3" | "aac" | "flac" | "ogg" | "opus"
            )
        })
}

fn system_time_ms(value: SystemTime) -> i64 {
    value
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn default_config_path() -> PathBuf {
    home().join("Library/Application Support/PersonalAudioLake/config.toml")
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_fixtures_are_supported() {
        personal_audio_lake_contracts::assert_contract_fixtures();
    }

    #[test]
    fn audio_filter_is_deliberately_narrow() {
        assert!(is_audio(Path::new("one.m4a")));
        assert!(!is_audio(Path::new("notes.txt")));
    }

    #[test]
    fn folder_leaf_satisfies_the_shared_source_contract() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("one.m4a"), b"fixture").unwrap();
        let source_id = root.path().display().to_string();
        let mut adapter = FolderSourceAdapter::new(root.path(), "watched-folder", "");
        let outputs = execute_recording_source(
            &mut adapter,
            SourceCommand {
                adapter_id: "watched-folder".into(),
                source_id: Some(source_id),
                operation: SourceOperation::Sync {
                    destination_uri: "lake://recordings".into(),
                    idle_timeout_ms: 0,
                },
            },
        )
        .unwrap();
        assert!(
            outputs
                .iter()
                .any(|output| matches!(output, SourceAdapterOutput::Artifact(_)))
        );
    }

    #[test]
    fn device_adapter_is_optional_for_file_folder_and_network_ingress() {
        let mut config = Config::default();
        assert!(!device_source_enabled(&config));
        assert!(!config.allow_source_pairing);
        let descriptor = source_catalog::find("pocket").unwrap();
        config.source_adapter_id = descriptor.id.into();
        config.source_adapter_binary = descriptor.executable.into();
        assert!(device_source_enabled(&config));
    }

    #[test]
    fn transfer_ingress_waits_for_source_adapter_readiness() {
        let health = |state| {
            Envelope::root(Message::WorkerHealth(WorkerHealth {
                role: WorkerRole::Source,
                state,
                detail: "fixture".into(),
                backlog: 0,
            }))
        };
        assert!(!should_start_transfer_ingress(
            Role::Source,
            &health(HealthState::Degraded),
            false
        ));
        assert!(should_start_transfer_ingress(
            Role::Source,
            &health(HealthState::Ready),
            false
        ));
        assert!(!should_start_transfer_ingress(
            Role::Source,
            &health(HealthState::Ready),
            true
        ));
        assert!(!should_start_transfer_ingress(
            Role::Lake,
            &health(HealthState::Ready),
            false
        ));
    }

    #[test]
    fn ephemeral_worker_health_never_enters_the_immutable_user_lake() {
        assert!(!forward_operational_fact_to_lake(
            Role::Source,
            &Message::WorkerHealth(WorkerHealth {
                role: WorkerRole::Source,
                state: HealthState::Ready,
                detail: "ready".into(),
                backlog: 0,
            })
        ));
        assert!(forward_operational_fact_to_lake(
            Role::Source,
            &Message::Failure(personal_audio_lake_contracts::Failure {
                worker: WorkerRole::Source,
                code: "fixture".into(),
                detail: "fixture".into(),
                retryable: true,
                failed_message_id: None,
            })
        ));
    }

    #[test]
    fn status_history_is_compacted_on_complete_line_boundaries() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("events.ndjson");
        fs::write(&path, "one\ntwo\nthree\nfour\n").unwrap();
        compact_status_tail(&path, 10, 12).unwrap();
        let compacted = fs::read_to_string(path).unwrap();
        assert_eq!(compacted, "three\nfour\n");
    }

    #[test]
    fn unchanged_source_polling_is_not_projected_into_the_immutable_lake() {
        let status = personal_audio_lake_contracts::SourceStatus {
            adapter_id: "fixture".into(),
            source_id: "one".into(),
            display_name: "Recorder".into(),
            state: SourceState::Ready,
            capabilities: vec![],
            attributes: Default::default(),
        };
        let mut states = HashMap::new();
        assert!(should_project_source_status(&mut states, &status, false));
        assert!(!should_project_source_status(&mut states, &status, false));

        let bound = personal_audio_lake_contracts::SourceStatus {
            state: SourceState::Bound,
            ..status
        };
        assert!(!should_project_source_status(&mut states, &bound, true));
    }

    #[test]
    fn supervisor_config_contains_only_adapter_identity_not_protocol_credentials() {
        let encoded = toml::to_string_pretty(&Config::default()).unwrap();
        assert!(!encoded.contains("binding_key"));
        assert!(!encoded.contains("binding_password"));
        assert!(!encoded.contains("service_uuid"));
        assert!(encoded.contains("source_adapter_id"));
    }
}
