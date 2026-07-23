use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use clap::{Parser, Subcommand};
use personal_audio_lake_contracts::{
    Envelope, Message, RecordingAvailable, SourceAdapter, SourceAdapterOutput, SourceCapability,
    SourceCommand, SourceOperation, SourceState, SourceStatus, execute_recording_source,
};
use uuid::Uuid;

#[derive(Parser)]
#[command(about = "Control and inspect the local personal-audio ingestion service")]
struct Cli {
    #[arg(long, env = "PERSONAL_AUDIO_LAKE_STATE_ROOT")]
    state_root: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Status {
        #[arg(long, default_value_t = 20)]
        lines: usize,
    },
    Pending,
    Approve {
        action_id: Uuid,
    },
    /// Replay the latest stored transcript for a recording through routing and notifications.
    Rederive {
        recording_id: Uuid,
    },
    Scan {
        #[arg(long, default_value = "configured")]
        adapter_id: String,
        #[arg(long, default_value_t = 12_000)]
        duration_ms: u64,
    },
    Ingest {
        path: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = cli.state_root.unwrap_or_else(default_state_root);
    match cli.command {
        Command::Status { lines } => status(&root, lines),
        Command::Pending => pending(&root),
        Command::Approve { action_id } => {
            enqueue(&root, Envelope::root(Message::ActionApproved { action_id }))
        }
        Command::Rederive { recording_id } => rederive(&root, recording_id),
        Command::Scan {
            adapter_id,
            duration_ms,
        } => enqueue(
            &root,
            Envelope::root(Message::SourceCommandRequested(SourceCommand {
                adapter_id,
                source_id: None,
                operation: SourceOperation::Discover {
                    duration_ms,
                    allow_pairing: true,
                },
            })),
        ),
        Command::Ingest { path } => enqueue(
            &root,
            Envelope::root(Message::RecordingAvailable(application_file(&path)?)),
        ),
    }
}

struct ApplicationFileSource {
    path: PathBuf,
}

impl SourceAdapter<RecordingAvailable> for ApplicationFileSource {
    type Error = anyhow::Error;

    fn adapter_id(&self) -> &str {
        "file"
    }

    fn source_id(&self) -> &str {
        "command-line"
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
            bail!("source command identity does not match application-file adapter");
        }
        if !matches!(command.operation, SourceOperation::Sync { .. }) {
            bail!("application-file adapter supports only sync");
        }
        Ok(vec![
            SourceAdapterOutput::Status(SourceStatus {
                adapter_id: self.adapter_id().into(),
                source_id: self.source_id().into(),
                display_name: "Command-line file".into(),
                state: SourceState::Ready,
                capabilities: self.capabilities().to_vec(),
                attributes: Default::default(),
            }),
            SourceAdapterOutput::Artifact(file_recording(&self.path)?),
        ])
    }
}

fn application_file(path: &Path) -> Result<RecordingAvailable> {
    let mut adapter = ApplicationFileSource {
        path: path.to_owned(),
    };
    execute_recording_source(
        &mut adapter,
        SourceCommand {
            adapter_id: "file".into(),
            source_id: Some("command-line".into()),
            operation: SourceOperation::Sync {
                destination_uri: "lake://recordings".into(),
                idle_timeout_ms: 0,
            },
        },
    )
    .map_err(anyhow::Error::msg)?
    .into_iter()
    .find_map(|output| match output {
        SourceAdapterOutput::Artifact(recording) => Some(recording),
        SourceAdapterOutput::Status(_) => None,
    })
    .context("application-file adapter returned no recording")
}

fn file_recording(path: &Path) -> Result<RecordingAvailable> {
    let path = path
        .canonicalize()
        .context("recording path does not exist")?;
    let recorded_at_ms = fs::metadata(&path)
        .and_then(|value| value.modified())
        .ok()
        .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|value| value.as_millis() as i64)
        .unwrap_or_else(|| Utc::now().timestamp_millis());
    Ok(RecordingAvailable {
        source_id: "command-line".into(),
        adapter_id: "file".into(),
        source_path: path.display().to_string(),
        recorded_at_ms,
    })
}

fn rederive(root: &Path, recording_id: Uuid) -> Result<()> {
    let events = fs::read_to_string(root.join("lake/manifests/events.ndjson"))
        .context("read immutable event manifest")?;
    let transcript = events
        .lines()
        .rev()
        .filter_map(|line| Envelope::from_json(line).ok())
        .find(|envelope| {
            matches!(
                &envelope.message,
                Message::TranscriptCompleted(run) if run.recording_id == recording_id
            )
        })
        .with_context(|| format!("no completed transcript for recording {recording_id}"))?;
    let Message::TranscriptCompleted(run) = transcript.message.clone() else {
        unreachable!("filtered to transcript_completed")
    };
    enqueue(
        root,
        Envelope::caused_by(&transcript, Message::TranscriptCompleted(run)),
    )
}

fn enqueue(root: &Path, envelope: Envelope) -> Result<()> {
    let inbox = root.join("control/inbox");
    fs::create_dir_all(&inbox)?;
    let target = inbox.join(format!("{}-{}.json", envelope.emitted_at_ms, envelope.id));
    let temporary = target.with_extension("tmp");
    fs::write(&temporary, envelope.to_ndjson()?)?;
    fs::rename(temporary, &target)?;
    println!("queued {}", envelope.id);
    Ok(())
}

fn status(root: &Path, count: usize) -> Result<()> {
    let value = fs::read_to_string(root.join("status/events.ndjson")).unwrap_or_default();
    let lines: Vec<_> = value.lines().collect();
    for line in lines.iter().skip(lines.len().saturating_sub(count)) {
        let parsed: serde_json::Value = serde_json::from_str(line)?;
        println!("{}", serde_json::to_string_pretty(&parsed)?);
    }
    Ok(())
}

fn pending(root: &Path) -> Result<()> {
    let directory = root.join("calendar/pending");
    if !directory.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.path().extension().and_then(|value| value.to_str()) == Some("json") {
            println!("{}", fs::read_to_string(entry.path())?);
        }
    }
    Ok(())
}

fn default_state_root() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Library/Application Support/PersonalAudioLake")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_fixtures_are_supported() {
        personal_audio_lake_contracts::assert_contract_fixtures();
    }

    #[test]
    fn application_file_leaf_satisfies_the_shared_source_contract() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("voice.m4a");
        fs::write(&path, b"fixture").unwrap();
        let recording = application_file(&path).unwrap();
        assert_eq!(recording.adapter_id, "file");
        assert_eq!(recording.source_id, "command-line");
    }

    #[test]
    fn rederive_reads_the_immutable_manifest() {
        let root = tempfile::tempdir().unwrap();
        let transcript = personal_audio_lake_contracts::contract_fixtures()
            .into_iter()
            .find(|envelope| matches!(envelope.message, Message::TranscriptCompleted(_)))
            .unwrap();
        let Message::TranscriptCompleted(run) = &transcript.message else {
            unreachable!()
        };
        let manifest = root.path().join("lake/manifests");
        fs::create_dir_all(&manifest).unwrap();
        fs::write(
            manifest.join("events.ndjson"),
            transcript.to_ndjson().unwrap(),
        )
        .unwrap();

        rederive(root.path(), run.recording_id).unwrap();
        let queued = fs::read_dir(root.path().join("control/inbox"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        let replay = Envelope::from_json(&fs::read_to_string(queued.path()).unwrap()).unwrap();
        assert!(matches!(replay.message, Message::TranscriptCompleted(_)));
        assert_eq!(replay.correlation_id, transcript.correlation_id);
        assert_ne!(replay.id, transcript.id);
    }
}
