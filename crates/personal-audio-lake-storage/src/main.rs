use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use clap::{Parser, Subcommand};
use personal_audio_lake_contracts::{
    AudioIngested, Envelope, Failure, HealthState, Message, RecordingAvailable, WorkerHealth,
    WorkerRole, ports::EventStore,
};
use reqwest::Client;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::time::{Duration, sleep};
use tracing::{error, info};
use uuid::Uuid;

const SCHEMA: &str = r#"
CREATE DATABASE IF NOT EXISTS personal_audio_lake;
CREATE TABLE IF NOT EXISTS personal_audio_lake.ingest_events (
    sequence UInt64,
    event_id UUID,
    correlation_id UUID,
    causation_id Nullable(UUID),
    emitted_at DateTime64(3, 'UTC'),
    message_type LowCardinality(String),
    payload_json String
) ENGINE = MergeTree ORDER BY (sequence, event_id);
CREATE TABLE IF NOT EXISTS personal_audio_lake.audio_objects (
    recording_id UUID,
    source_id String,
    sha256 FixedString(64),
    relative_path String,
    byte_len UInt64,
    recorded_at DateTime64(3, 'UTC'),
    ingested_at DateTime64(3, 'UTC'),
    introducing_event_id UUID
) ENGINE = ReplacingMergeTree(ingested_at) ORDER BY (recording_id, sha256);
ALTER TABLE personal_audio_lake.audio_objects ADD COLUMN IF NOT EXISTS source_id String AFTER recording_id;
CREATE TABLE IF NOT EXISTS personal_audio_lake.transcript_segments (
    recording_id UUID,
    run_id UUID,
    ordinal UInt32,
    start_ms UInt64,
    end_ms UInt64,
    speaker Nullable(String),
    text String,
    confidence Nullable(Float32),
    language Nullable(String),
    model String,
    model_version String,
    completed_at DateTime64(3, 'UTC')
) ENGINE = MergeTree ORDER BY (recording_id, run_id, ordinal);
CREATE TABLE IF NOT EXISTS personal_audio_lake.action_facts (
    action_id UUID,
    recording_id UUID,
    transcript_run_id UUID,
    kind LowCardinality(String),
    title String,
    due_at Nullable(DateTime64(3, 'UTC')),
    timezone String,
    confidence Float32,
    evidence_json String,
    extractor_version String,
    observed_at DateTime64(3, 'UTC')
) ENGINE = ReplacingMergeTree(observed_at) ORDER BY (action_id, extractor_version);
CREATE TABLE IF NOT EXISTS personal_audio_lake.action_deliveries (
    action_id UUID,
    external_id String,
    target LowCardinality(String),
    delivered_at DateTime64(3, 'UTC'),
    event_id UUID
) ENGINE = ReplacingMergeTree(delivered_at) ORDER BY (action_id, target);
CREATE TABLE IF NOT EXISTS personal_audio_lake.transcript_annotations (
    annotation_id UUID,
    recording_id UUID,
    transcript_run_id UUID,
    ordinal UInt32,
    replacement_text String,
    reason Nullable(String),
    annotated_at DateTime64(3, 'UTC'),
    event_id UUID
) ENGINE = ReplacingMergeTree(annotated_at) ORDER BY (recording_id, transcript_run_id, ordinal);
CREATE TABLE IF NOT EXISTS personal_audio_lake.action_reviews (
    action_id UUID,
    decision LowCardinality(String),
    reason String,
    reviewed_at DateTime64(3, 'UTC'),
    event_id UUID
) ENGINE = ReplacingMergeTree(reviewed_at) ORDER BY action_id;
CREATE TABLE IF NOT EXISTS personal_audio_lake.service_deliveries (
    service_id String,
    source_event_id UUID,
    delivered_at DateTime64(3, 'UTC'),
    target String,
    receipt String,
    event_id UUID
) ENGINE = ReplacingMergeTree(delivered_at) ORDER BY (service_id, source_event_id);
CREATE TABLE IF NOT EXISTS personal_audio_lake.notification_deliveries (
    recording_id UUID,
    delivered_at DateTime64(3, 'UTC'),
    summary String,
    hooks_json String,
    event_id UUID
) ENGINE = ReplacingMergeTree(delivered_at) ORDER BY recording_id;
CREATE TABLE IF NOT EXISTS personal_audio_lake.recording_reviews (
    review_id UUID,
    recording_id UUID,
    outcome LowCardinality(String),
    note Nullable(String),
    reviewed_at DateTime64(3, 'UTC'),
    event_id UUID
) ENGINE = ReplacingMergeTree(reviewed_at) ORDER BY recording_id;
"#;

#[derive(Parser)]
struct Cli {
    #[arg(long, env = "PERSONAL_AUDIO_LAKE_ROOT")]
    lake_root: Option<PathBuf>,
    #[arg(
        long,
        env = "PERSONAL_AUDIO_LAKE_CLICKHOUSE_URL",
        default_value = "http://127.0.0.1:8123"
    )]
    clickhouse_url: String,
    #[arg(long, default_value = "personal_audio_lake")]
    database: String,
    #[arg(
        long,
        env = "PERSONAL_AUDIO_LAKE_MANIFEST_ONLY",
        default_value_t = false
    )]
    manifest_only: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    Init,
    /// Recreate all ClickHouse projections from the immutable raw manifest.
    Rebuild,
    /// Materialize immutable transcript objects from an older event manifest without changing
    /// ClickHouse. This is safe to run while upgrading an existing installation.
    BackfillRaw,
    Worker,
    Ingest {
        path: PathBuf,
        #[arg(long, default_value = "manual")]
        source_id: String,
    },
}

struct Lake {
    root: PathBuf,
    clickhouse_url: String,
    database: String,
    manifest_only: bool,
    http: Client,
}

#[derive(Serialize)]
struct EventRow<'a> {
    sequence: u64,
    event_id: Uuid,
    correlation_id: Uuid,
    causation_id: Option<Uuid>,
    emitted_at: String,
    message_type: &'a str,
    payload_json: String,
}

impl EventStore<Envelope> for Lake {
    type Error = anyhow::Error;
    type Receipt = u64;

    fn append(&mut self, envelope: &Envelope) -> Result<u64> {
        let path = self.root.join("manifests/events.ndjson");
        let sequence_path = self.root.join("manifests/sequence");
        let sequence = fs::read_to_string(&sequence_path)
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .unwrap_or(0)
            + 1;
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        file.write_all(envelope.to_ndjson()?.as_bytes())?;
        file.sync_data()?;
        let tmp = sequence_path.with_extension("tmp");
        fs::write(&tmp, format!("{sequence}\n"))?;
        fs::rename(tmp, sequence_path)?;
        Ok(sequence)
    }
}

fn event_row(sequence: u64, envelope: &Envelope) -> Result<EventRow<'_>> {
    Ok(EventRow {
        sequence,
        event_id: envelope.id,
        correlation_id: envelope.correlation_id,
        causation_id: envelope.causation_id,
        emitted_at: millis_datetime(envelope.emitted_at_ms),
        message_type: message_type(&envelope.message),
        payload_json: serde_json::to_string(&envelope.message)?,
    })
}

impl Lake {
    fn new(root: PathBuf, clickhouse_url: String, database: String, manifest_only: bool) -> Self {
        assert!(
            !database.is_empty()
                && database
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
            "ClickHouse database name must be an identifier"
        );
        Self {
            root,
            clickhouse_url,
            database,
            manifest_only,
            http: Client::new(),
        }
    }

    async fn initialize(&self) -> Result<()> {
        for path in [
            self.root.join("objects/audio"),
            self.root.join("objects/derived"),
            self.root.join("clickhouse"),
            self.root.join("manifests"),
            self.root.join("quarantine"),
        ] {
            fs::create_dir_all(path)?;
        }
        if !self.manifest_only {
            let mut last_error = None;
            for _ in 0..60 {
                match self.initialize_schema().await {
                    Ok(()) => return Ok(()),
                    Err(error) => {
                        error!(error = %error, "waiting for ClickHouse schema initialization");
                        last_error = Some(error);
                        sleep(Duration::from_secs(1)).await;
                    }
                }
            }
            return Err(last_error.expect("ClickHouse retry records an error"))
                .context("initialize ClickHouse schema after 60 seconds");
        }
        Ok(())
    }

    async fn initialize_schema(&self) -> Result<()> {
        let schema = SCHEMA
            .replace(
                "CREATE DATABASE IF NOT EXISTS personal_audio_lake",
                &format!("CREATE DATABASE IF NOT EXISTS {}", self.database),
            )
            .replace("personal_audio_lake.", &format!("{}.", self.database));
        for statement in schema
            .split(';')
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            self.sql(statement).await?;
        }
        Ok(())
    }

    async fn sql(&self, query: &str) -> Result<()> {
        let response = self
            .http
            .post(&self.clickhouse_url)
            .body(query.to_owned())
            .send()
            .await?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("ClickHouse {status}: {body}");
        }
        Ok(())
    }

    async fn record_envelope(&self, sequence: u64, envelope: &Envelope) -> Result<()> {
        if self.manifest_only {
            return Ok(());
        }
        let row = event_row(sequence, envelope)?;
        self.insert_json("personal_audio_lake.ingest_events", &row)
            .await
    }

    async fn record_envelopes(&self, envelopes: &[Envelope]) -> Result<()> {
        let mut body = String::new();
        for (index, envelope) in envelopes.iter().enumerate() {
            body.push_str(&serde_json::to_string(&event_row(
                index as u64 + 1,
                envelope,
            )?)?);
            body.push('\n');
        }
        self.insert_json_lines("personal_audio_lake.ingest_events", body)
            .await
    }

    async fn insert_json_lines(&self, table: &str, body: String) -> Result<()> {
        let table = table.replacen("personal_audio_lake.", &format!("{}.", self.database), 1);
        let response = self
            .http
            .post(&self.clickhouse_url)
            .query(&[("query", format!("INSERT INTO {table} FORMAT JSONEachRow"))])
            .body(body)
            .send()
            .await?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("ClickHouse insert into {table} failed ({status}): {body}");
        }
        Ok(())
    }

    async fn insert_json<T: Serialize>(&self, table: &str, row: &T) -> Result<()> {
        let table = table.replacen("personal_audio_lake.", &format!("{}.", self.database), 1);
        let response = self
            .http
            .post(&self.clickhouse_url)
            .query(&[("query", format!("INSERT INTO {table} FORMAT JSONEachRow"))])
            .body(format!("{}\n", serde_json::to_string(row)?))
            .send()
            .await?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("ClickHouse insert into {table} failed ({status}): {body}");
        }
        Ok(())
    }

    async fn process(&mut self, envelope: &Envelope) -> Result<Option<Envelope>> {
        let sequence = self.append(envelope)?;
        self.record_envelope(sequence, envelope).await?;
        self.project(envelope).await?;
        match &envelope.message {
            Message::RecordingAvailable(recording) => {
                let audio = self.ingest_audio(envelope, recording).await?;
                Ok(Some(Envelope::caused_by(
                    envelope,
                    Message::AudioIngested(audio),
                )))
            }
            _ => Ok(None),
        }
    }

    async fn project(&self, envelope: &Envelope) -> Result<()> {
        match &envelope.message {
            Message::AudioIngested(audio) if !self.manifest_only => {
                #[derive(Serialize)]
                struct AudioRow<'a> {
                    recording_id: Uuid,
                    source_id: &'a str,
                    sha256: &'a str,
                    relative_path: &'a str,
                    byte_len: u64,
                    recorded_at: String,
                    ingested_at: String,
                    introducing_event_id: Uuid,
                }
                self.insert_json(
                    "personal_audio_lake.audio_objects",
                    &AudioRow {
                        recording_id: audio.recording_id,
                        source_id: &audio.source_id,
                        sha256: &audio.sha256,
                        relative_path: &audio.relative_path,
                        byte_len: audio.byte_len,
                        recorded_at: millis_datetime(audio.recorded_at_ms),
                        ingested_at: millis_datetime(audio.ingested_at_ms),
                        introducing_event_id: envelope.id,
                    },
                )
                .await?;
            }
            Message::TranscriptCompleted(run) => {
                self.persist_raw_transcript(run)?;
                if self.manifest_only {
                    return Ok(());
                }
                for segment in &run.segments {
                    #[derive(Serialize)]
                    struct SegmentRow<'a> {
                        recording_id: Uuid,
                        run_id: Uuid,
                        ordinal: u32,
                        start_ms: u64,
                        end_ms: u64,
                        speaker: &'a Option<String>,
                        text: &'a str,
                        confidence: Option<f32>,
                        language: &'a Option<String>,
                        model: &'a str,
                        model_version: &'a str,
                        completed_at: String,
                    }
                    self.insert_json(
                        "personal_audio_lake.transcript_segments",
                        &SegmentRow {
                            recording_id: run.recording_id,
                            run_id: run.run_id,
                            ordinal: segment.ordinal,
                            start_ms: segment.start_ms,
                            end_ms: segment.end_ms,
                            speaker: &segment.speaker,
                            text: &segment.text,
                            confidence: segment.confidence,
                            language: &segment.language,
                            model: &run.model,
                            model_version: &run.model_version,
                            completed_at: millis_datetime(run.completed_at_ms),
                        },
                    )
                    .await?;
                }
            }
            Message::ActionProposed(action) if !self.manifest_only => {
                #[derive(Serialize)]
                struct ActionRow<'a> {
                    action_id: Uuid,
                    recording_id: Uuid,
                    transcript_run_id: Uuid,
                    kind: String,
                    title: &'a str,
                    due_at: Option<String>,
                    timezone: &'a str,
                    confidence: f32,
                    evidence_json: String,
                    extractor_version: &'a str,
                    observed_at: String,
                }
                self.insert_json(
                    "personal_audio_lake.action_facts",
                    &ActionRow {
                        action_id: action.action_id,
                        recording_id: action.recording_id,
                        transcript_run_id: action.transcript_run_id,
                        kind: format!("{:?}", action.kind).to_lowercase(),
                        title: &action.title,
                        due_at: action.due_at_ms.map(millis_datetime),
                        timezone: &action.timezone,
                        confidence: action.confidence,
                        evidence_json: serde_json::to_string(&action.evidence)?,
                        extractor_version: &action.extractor_version,
                        observed_at: millis_datetime(envelope.emitted_at_ms),
                    },
                )
                .await?;
            }
            Message::ActionApproved { action_id } if !self.manifest_only => {
                self.insert_review(envelope, *action_id, "approved", "")
                    .await?;
            }
            Message::ActionRejected { action_id, reason } if !self.manifest_only => {
                self.insert_review(envelope, *action_id, "rejected", reason)
                    .await?;
            }
            Message::ActionDelivered(delivery) if !self.manifest_only => {
                #[derive(Serialize)]
                struct DeliveryRow<'a> {
                    action_id: Uuid,
                    external_id: &'a str,
                    target: &'a str,
                    delivered_at: String,
                    event_id: Uuid,
                }
                self.insert_json(
                    "personal_audio_lake.action_deliveries",
                    &DeliveryRow {
                        action_id: delivery.action_id,
                        external_id: &delivery.external_id,
                        target: &delivery.target,
                        delivered_at: millis_datetime(delivery.delivered_at_ms),
                        event_id: envelope.id,
                    },
                )
                .await?;
            }
            Message::TranscriptAnnotated(annotation) if !self.manifest_only => {
                #[derive(Serialize)]
                struct AnnotationRow<'a> {
                    annotation_id: Uuid,
                    recording_id: Uuid,
                    transcript_run_id: Uuid,
                    ordinal: u32,
                    replacement_text: &'a str,
                    reason: &'a Option<String>,
                    annotated_at: String,
                    event_id: Uuid,
                }
                self.insert_json(
                    "personal_audio_lake.transcript_annotations",
                    &AnnotationRow {
                        annotation_id: annotation.annotation_id,
                        recording_id: annotation.recording_id,
                        transcript_run_id: annotation.transcript_run_id,
                        ordinal: annotation.ordinal,
                        replacement_text: &annotation.replacement_text,
                        reason: &annotation.reason,
                        annotated_at: millis_datetime(annotation.annotated_at_ms),
                        event_id: envelope.id,
                    },
                )
                .await?;
            }
            Message::ServiceDelivered(delivery) if !self.manifest_only => {
                #[derive(Serialize)]
                struct DeliveryRow<'a> {
                    service_id: &'a str,
                    source_event_id: Uuid,
                    delivered_at: String,
                    target: &'a str,
                    receipt: &'a str,
                    event_id: Uuid,
                }
                self.insert_json(
                    "personal_audio_lake.service_deliveries",
                    &DeliveryRow {
                        service_id: &delivery.service_id,
                        source_event_id: delivery.source_event_id,
                        delivered_at: millis_datetime(delivery.delivered_at_ms),
                        target: &delivery.target,
                        receipt: &delivery.receipt,
                        event_id: envelope.id,
                    },
                )
                .await?;
            }
            Message::NotificationDelivered(delivery) if !self.manifest_only => {
                #[derive(Serialize)]
                struct NotificationRow<'a> {
                    recording_id: Uuid,
                    delivered_at: String,
                    summary: &'a str,
                    hooks_json: String,
                    event_id: Uuid,
                }
                self.insert_json(
                    "personal_audio_lake.notification_deliveries",
                    &NotificationRow {
                        recording_id: delivery.recording_id,
                        delivered_at: millis_datetime(delivery.delivered_at_ms),
                        summary: &delivery.summary,
                        hooks_json: serde_json::to_string(&delivery.hooks)?,
                        event_id: envelope.id,
                    },
                )
                .await?;
            }
            Message::RecordingReviewed(review) if !self.manifest_only => {
                #[derive(Serialize)]
                struct RecordingReviewRow<'a> {
                    review_id: Uuid,
                    recording_id: Uuid,
                    outcome: &'a str,
                    note: &'a Option<String>,
                    reviewed_at: String,
                    event_id: Uuid,
                }
                self.insert_json(
                    "personal_audio_lake.recording_reviews",
                    &RecordingReviewRow {
                        review_id: review.review_id,
                        recording_id: review.recording_id,
                        outcome: match review.outcome {
                            personal_audio_lake_contracts::ReviewOutcome::Confirmed => "confirmed",
                            personal_audio_lake_contracts::ReviewOutcome::NeedsAttention => {
                                "needs_attention"
                            }
                        },
                        note: &review.note,
                        reviewed_at: millis_datetime(review.reviewed_at_ms),
                        event_id: envelope.id,
                    },
                )
                .await?;
            }
            _ => {}
        }
        Ok(())
    }

    fn persist_raw_transcript(
        &self,
        run: &personal_audio_lake_contracts::TranscriptCompleted,
    ) -> Result<()> {
        let directory = self
            .root
            .join("objects/transcripts")
            .join(run.recording_id.to_string());
        fs::create_dir_all(&directory)?;
        let target = directory.join(format!("{}.json", run.run_id));
        if target.exists() {
            return Ok(());
        }
        let temporary = target.with_extension("json.partial");
        let bytes = serde_json::to_vec_pretty(run)?;
        fs::write(&temporary, &bytes)?;
        File::open(&temporary)?.sync_all()?;
        fs::rename(temporary, target)?;
        Ok(())
    }

    async fn insert_review(
        &self,
        envelope: &Envelope,
        action_id: Uuid,
        decision: &str,
        reason: &str,
    ) -> Result<()> {
        #[derive(Serialize)]
        struct ReviewRow<'a> {
            action_id: Uuid,
            decision: &'a str,
            reason: &'a str,
            reviewed_at: String,
            event_id: Uuid,
        }
        self.insert_json(
            "personal_audio_lake.action_reviews",
            &ReviewRow {
                action_id,
                decision,
                reason,
                reviewed_at: millis_datetime(envelope.emitted_at_ms),
                event_id: envelope.id,
            },
        )
        .await
    }

    async fn rebuild(&self) -> Result<()> {
        self.initialize().await?;
        if self.manifest_only {
            bail!("cannot rebuild ClickHouse in manifest-only mode");
        }
        for table in [
            "service_deliveries",
            "action_reviews",
            "transcript_annotations",
            "action_deliveries",
            "action_facts",
            "transcript_segments",
            "audio_objects",
            "ingest_events",
        ] {
            self.sql(&format!(
                "TRUNCATE TABLE IF EXISTS {}.{table}",
                self.database
            ))
            .await?;
        }
        let manifest = self.root.join("manifests/events.ndjson");
        let file = File::open(&manifest)
            .with_context(|| format!("open immutable manifest {}", manifest.display()))?;
        let envelopes = io::BufReader::new(file)
            .lines()
            .map(|line| -> Result<Envelope> { Ok(Envelope::from_json(&line?)?) })
            .collect::<Result<Vec<_>>>()?;
        self.record_envelopes(&envelopes).await?;
        for envelope in &envelopes {
            self.project(envelope).await?;
        }
        Ok(())
    }

    fn backfill_raw(&self) -> Result<u64> {
        let manifest = self.root.join("manifests/events.ndjson");
        let file = File::open(&manifest)
            .with_context(|| format!("open immutable manifest {}", manifest.display()))?;
        let mut count = 0;
        for line in io::BufReader::new(file).lines() {
            let envelope = Envelope::from_json(&line?)?;
            if let Message::TranscriptCompleted(run) = &envelope.message {
                self.persist_raw_transcript(run)?;
                count += 1;
            }
        }
        Ok(count)
    }

    async fn ingest_audio(
        &self,
        _envelope: &Envelope,
        recording: &RecordingAvailable,
    ) -> Result<AudioIngested> {
        let source = Path::new(&recording.source_path);
        let (sha256, byte_len) = hash_file(source)?;
        let extension = source
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("bin");
        let relative = PathBuf::from("objects/audio")
            .join(&sha256[..2])
            .join(format!("{sha256}.{extension}"));
        let destination = self.root.join(&relative);
        if !destination.exists() {
            fs::create_dir_all(destination.parent().expect("audio path has parent"))?;
            let staging = destination.with_extension(format!("{extension}.partial"));
            fs::copy(source, &staging)?;
            File::open(&staging)?.sync_all()?;
            let (copied_hash, _) = hash_file(&staging)?;
            if copied_hash != sha256 {
                let _ = fs::remove_file(&staging);
                bail!("audio changed while copying: {}", source.display());
            }
            fs::rename(staging, &destination)?;
        }
        let ingested_at_ms = Utc::now().timestamp_millis();
        let audio = AudioIngested {
            recording_id: recording.stable_id(&sha256),
            source_id: recording.source_id.clone(),
            sha256,
            relative_path: relative.to_string_lossy().into_owned(),
            byte_len,
            recorded_at_ms: recording.recorded_at_ms,
            ingested_at_ms,
        };
        Ok(audio)
    }
}

fn default_lake_root() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join("Library/Application Support/PersonalAudioLake/lake")
}

fn hash_file(path: &Path) -> Result<(String, u64)> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];
    let mut bytes = 0_u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        bytes += read as u64;
    }
    Ok((hex::encode(hasher.finalize()), bytes))
}

fn millis_datetime(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or_else(Utc::now)
        .format("%Y-%m-%d %H:%M:%S%.3f")
        .to_string()
}

fn message_type(message: &Message) -> &'static str {
    match message {
        Message::StartWorker { .. } => "start_worker",
        Message::StopWorker { .. } => "stop_worker",
        Message::WorkerHealth(_) => "worker_health",
        Message::SourceAdapterSelectionRequested { .. } => "source_adapter_selection_requested",
        Message::SourceCommandRequested(_) => "source_command_requested",
        Message::SourceStatusChanged(_) => "source_status_changed",
        Message::ReleaseSourceRequested => "release_source_requested",
        Message::RecordingAvailable(_) => "recording_available",
        Message::AudioIngested(_) => "audio_ingested",
        Message::TranscriptionRequested(_) => "transcription_requested",
        Message::TranscriptCompleted(_) => "transcript_completed",
        Message::DerivationRequested(_) => "derivation_requested",
        Message::ActionProposed(_) => "action_proposed",
        Message::ActionRevisionRequested(_) => "action_revision_requested",
        Message::ActionApproved { .. } => "action_approved",
        Message::ActionRejected { .. } => "action_rejected",
        Message::ActionDelivered(_) => "action_delivered",
        Message::ActionUndoRequested { .. } => "action_undo_requested",
        Message::ActionUndone(_) => "action_undone",
        Message::DerivationCompleted(_) => "derivation_completed",
        Message::NotificationDelivered(_) => "notification_delivered",
        Message::RecordingReviewed(_) => "recording_reviewed",
        Message::TranscriptAnnotated(_) => "transcript_annotated",
        Message::WorkflowRunRequested(_) => "workflow_run_requested",
        Message::WorkflowRoutingRequested(_) => "workflow_routing_requested",
        Message::WorkflowTransitionAdjusted(_) => "workflow_transition_adjusted",
        Message::WorkflowEffectUndoRequested(_) => "workflow_effect_undo_requested",
        Message::WorkflowRunConfirmed(_) => "workflow_run_confirmed",
        Message::WorkflowRunRecorded(_) => "workflow_run_recorded",
        Message::WorkflowEffectUndone(_) => "workflow_effect_undone",
        Message::ServiceDelivered(_) => "service_delivered",
        Message::Failure(_) => "failure",
        Message::LegacySourceEvent(_) => "legacy_source_event",
    }
}

fn emit(envelope: &Envelope) -> Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(envelope.to_ndjson()?.as_bytes())?;
    stdout.flush()?;
    Ok(())
}

async fn worker(lake: &mut Lake) -> Result<()> {
    lake.initialize().await?;
    emit(&Envelope::root(Message::WorkerHealth(WorkerHealth {
        role: WorkerRole::Lake,
        state: HealthState::Ready,
        detail: lake.root.display().to_string(),
        backlog: 0,
    })))?;
    for line in io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match Envelope::from_json(&line) {
            Ok(envelope) => match lake.process(&envelope).await {
                Ok(Some(output)) => emit(&output)?,
                Ok(None) => {}
                Err(source) => {
                    error!(error = %source, "lake processing failed");
                    emit(&Envelope::caused_by(
                        &envelope,
                        Message::Failure(Failure {
                            worker: WorkerRole::Lake,
                            code: "lake_processing_failed".into(),
                            detail: source.to_string(),
                            retryable: true,
                            failed_message_id: Some(envelope.id),
                        }),
                    ))?;
                }
            },
            Err(source) => error!(error = %source, "invalid contract input"),
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_writer(io::stderr).init();
    let cli = Cli::parse();
    let root = cli.lake_root.unwrap_or_else(default_lake_root);
    let mut lake = Lake::new(root, cli.clickhouse_url, cli.database, cli.manifest_only);
    match cli.command.unwrap_or(Command::Worker) {
        Command::Init => {
            lake.initialize().await?;
            info!(root = %lake.root.display(), "lake initialized");
        }
        Command::Rebuild => {
            lake.rebuild().await?;
            info!(root = %lake.root.display(), "lakehouse rebuilt from immutable manifest");
        }
        Command::BackfillRaw => {
            lake.initialize().await?;
            let count = lake.backfill_raw()?;
            info!(count, root = %lake.root.display(), "raw transcript objects backfilled");
        }
        Command::Worker => worker(&mut lake).await?,
        Command::Ingest { path, source_id } => {
            lake.initialize().await?;
            let timestamp = fs::metadata(&path)
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_millis() as i64)
                .unwrap_or_else(|| Utc::now().timestamp_millis());
            let input = Envelope::root(Message::RecordingAvailable(RecordingAvailable {
                source_id,
                adapter_id: "command-line".into(),
                source_path: path.display().to_string(),
                recorded_at_ms: timestamp,
            }));
            if let Some(output) = lake.process(&input).await? {
                emit(&output)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use personal_audio_lake_contracts::assert_contract_fixtures;

    #[test]
    fn independently_accepts_contract_fixtures() {
        assert_contract_fixtures();
    }

    #[tokio::test]
    async fn repeated_audio_is_content_addressed_once() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.wav");
        fs::write(&source, b"fixture audio").unwrap();
        let mut lake = Lake::new(
            temp.path().join("lake"),
            String::new(),
            "personal_audio_lake_test".into(),
            true,
        );
        lake.initialize().await.unwrap();
        let message = Envelope::root(Message::RecordingAvailable(RecordingAvailable {
            source_id: "fixture".into(),
            adapter_id: "fixture-adapter".into(),
            source_path: source.display().to_string(),
            recorded_at_ms: 1_700_000_000_000,
        }));
        let first = lake.process(&message).await.unwrap().unwrap();
        let second = lake.process(&message).await.unwrap().unwrap();
        let (Message::AudioIngested(first), Message::AudioIngested(second)) =
            (first.message, second.message)
        else {
            panic!("lake did not emit audio_ingested");
        };
        assert_eq!(first.recording_id, second.recording_id);
        assert_eq!(first.sha256, second.sha256);
        assert_eq!(first.relative_path, second.relative_path);
        let count = walkdir_count(&temp.path().join("lake/objects/audio"));
        assert_eq!(count, 1);
    }

    fn walkdir_count(root: &Path) -> usize {
        fs::read_dir(root)
            .unwrap()
            .flat_map(|entry| fs::read_dir(entry.unwrap().path()).unwrap())
            .filter_map(Result::ok)
            .filter(|entry| entry.path().is_file())
            .count()
    }
}
