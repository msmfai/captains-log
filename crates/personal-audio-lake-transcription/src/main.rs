use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use personal_audio_lake_contracts::{
    AudioIngested, Envelope, Failure, HealthState, Message, TranscriptCompleted, TranscriptSegment,
    WorkerHealth, WorkerRole, ports::Transcriber,
};
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

const RUN_NAMESPACE: Uuid = Uuid::from_u128(0x794d935d_8886_4e5a_88d1_c922be51a397);

#[derive(Debug, Deserialize)]
struct Sidecar {
    #[serde(default = "fixture_model")]
    model: String,
    #[serde(default = "fixture_version")]
    model_version: String,
    segments: Vec<TranscriptSegment>,
}

fn fixture_model() -> String {
    "sidecar".into()
}

fn fixture_version() -> String {
    "1".into()
}

fn default_lake_root() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join("Library/Application Support/PersonalAudioLake/lake")
}

struct LocalTranscriber {
    lake_root: PathBuf,
}

impl Transcriber<AudioIngested, TranscriptCompleted> for LocalTranscriber {
    type Error = anyhow::Error;

    fn transcribe(&mut self, audio: AudioIngested) -> Result<TranscriptCompleted> {
        transcribe(&audio, &self.lake_root)
    }
}

fn transcribe(audio: &AudioIngested, lake_root: &Path) -> Result<TranscriptCompleted> {
    let started_at_ms = Utc::now().timestamp_millis();
    let path = lake_root.join(&audio.relative_path);
    let backend = std::env::var("PERSONAL_AUDIO_LAKE_TRANSCRIBE_BACKEND")
        .unwrap_or_else(|_| "whisper-cli".into());
    let (model, model_version, segments, provider_payload) = match backend.as_str() {
        "sidecar" => {
            let sidecar_path = PathBuf::from(format!("{}.transcript.json", path.display()));
            let parsed: Sidecar = serde_json::from_slice(
                &fs::read(&sidecar_path)
                    .with_context(|| format!("read {}", sidecar_path.display()))?,
            )?;
            let payload = serde_json::to_value(&parsed.segments)?;
            (
                parsed.model,
                parsed.model_version,
                parsed.segments,
                Some(payload),
            )
        }
        "whisper-cli" => run_whisper(&path)?,
        other => bail!("unknown transcription backend {other}"),
    };
    if segments.is_empty() {
        bail!("transcription produced no segments");
    }
    let identity = format!("{}:{model}:{model_version}", audio.recording_id);
    Ok(TranscriptCompleted {
        recording_id: audio.recording_id,
        run_id: Uuid::new_v5(&RUN_NAMESPACE, identity.as_bytes()),
        recorded_at_ms: audio.recorded_at_ms,
        model,
        model_version,
        started_at_ms,
        completed_at_ms: Utc::now().timestamp_millis(),
        segments,
        provider_payload,
    })
}

fn already_transcribed(audio: &AudioIngested, lake_root: &Path) -> bool {
    let directory = lake_root
        .join("objects/transcripts")
        .join(audio.recording_id.to_string());
    fs::read_dir(directory).is_ok_and(|entries| {
        entries
            .filter_map(Result::ok)
            .any(|entry| entry.path().extension().and_then(|value| value.to_str()) == Some("json"))
    })
}

fn run_whisper(path: &Path) -> Result<(String, String, Vec<TranscriptSegment>, Option<Value>)> {
    let bin =
        std::env::var("PERSONAL_AUDIO_LAKE_WHISPER_BIN").unwrap_or_else(|_| "whisper-cli".into());
    let ffmpeg =
        std::env::var("PERSONAL_AUDIO_LAKE_FFMPEG_BIN").unwrap_or_else(|_| "ffmpeg".into());
    let model_path = std::env::var("PERSONAL_AUDIO_LAKE_WHISPER_MODEL")
        .context("PERSONAL_AUDIO_LAKE_WHISPER_MODEL must point to a local whisper.cpp model")?;
    let language =
        std::env::var("PERSONAL_AUDIO_LAKE_TRANSCRIBE_LANGUAGE").unwrap_or_else(|_| "auto".into());
    let temp = tempfile::tempdir()?;
    let normalized = temp.path().join("audio.wav");
    let normalize_status = Command::new(&ffmpeg)
        .args(["-nostdin", "-loglevel", "error", "-y", "-i"])
        .arg(path)
        .args(["-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le"])
        .arg(&normalized)
        .status()
        .with_context(|| format!("launch {ffmpeg}"))?;
    if !normalize_status.success() {
        bail!("{ffmpeg} could not normalize {}", path.display());
    }
    let output_base = temp.path().join("transcript");
    let status = Command::new(&bin)
        .args(["-m", &model_path, "-f"])
        .arg(&normalized)
        .args(["-oj", "-of"])
        .arg(&output_base)
        .args(["-l", &language, "--no-prints"])
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("launch {bin}"))?;
    if !status.success() {
        bail!("{bin} exited with {status}");
    }
    let output_path = output_base.with_extension("json");
    let json: Value = serde_json::from_slice(&fs::read(&output_path)?)?;
    let model_version = json
        .pointer("/model/type")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    let detected_language = json
        .pointer("/result/language")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let items = json
        .get("transcription")
        .and_then(Value::as_array)
        .context("whisper output has no transcription array")?;
    let segments = items
        .iter()
        .enumerate()
        .filter_map(|(ordinal, item)| {
            let text = item.get("text")?.as_str()?.trim().to_owned();
            if text.is_empty() {
                return None;
            }
            let offsets = item.get("offsets")?;
            Some(TranscriptSegment {
                ordinal: ordinal as u32,
                start_ms: offsets.get("from").and_then(Value::as_u64).unwrap_or(0) * 10,
                end_ms: offsets.get("to").and_then(Value::as_u64).unwrap_or(0) * 10,
                speaker: None,
                text,
                confidence: None,
                language: detected_language.clone(),
            })
        })
        .collect();
    Ok(("whisper.cpp".into(), model_version, segments, Some(json)))
}

fn emit(envelope: &Envelope) -> Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(envelope.to_ndjson()?.as_bytes())?;
    stdout.flush()?;
    Ok(())
}

fn transcription_readiness() -> Result<String> {
    let backend = std::env::var("PERSONAL_AUDIO_LAKE_TRANSCRIBE_BACKEND")
        .unwrap_or_else(|_| "whisper-cli".into());
    if backend == "sidecar" {
        return Ok("sidecar transcript adapter ready".into());
    }
    if backend != "whisper-cli" {
        bail!("unknown transcription backend {backend}");
    }
    let bin =
        std::env::var("PERSONAL_AUDIO_LAKE_WHISPER_BIN").unwrap_or_else(|_| "whisper-cli".into());
    let ffmpeg =
        std::env::var("PERSONAL_AUDIO_LAKE_FFMPEG_BIN").unwrap_or_else(|_| "ffmpeg".into());
    if !command_available(&bin) {
        bail!("whisper executable is unavailable: {bin}");
    }
    if !command_available(&ffmpeg) {
        bail!("ffmpeg executable is unavailable: {ffmpeg}");
    }
    let model = std::env::var_os("PERSONAL_AUDIO_LAKE_WHISPER_MODEL")
        .map(PathBuf::from)
        .context("PERSONAL_AUDIO_LAKE_WHISPER_MODEL is not configured")?;
    let metadata = fs::metadata(&model)
        .with_context(|| format!("Whisper model is unavailable: {}", model.display()))?;
    if !metadata.is_file() || metadata.len() == 0 {
        bail!("Whisper model is not a non-empty file: {}", model.display());
    }
    Ok(format!(
        "local whisper.cpp model ready: {} bytes",
        metadata.len()
    ))
}

fn command_available(value: &str) -> bool {
    let path = Path::new(value);
    if path.components().count() > 1 {
        return path.is_file();
    }
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|directory| directory.join(value).is_file())
    })
}

fn main() -> Result<()> {
    let lake_root = std::env::var_os("PERSONAL_AUDIO_LAKE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(default_lake_root);
    let mut transcriber = LocalTranscriber {
        lake_root: lake_root.clone(),
    };
    let readiness = transcription_readiness();
    emit(&Envelope::root(Message::WorkerHealth(WorkerHealth {
        role: WorkerRole::Transcribe,
        state: if readiness.is_ok() {
            HealthState::Ready
        } else {
            HealthState::Degraded
        },
        detail: readiness.unwrap_or_else(|error| format!("transcription unavailable: {error:#}")),
        backlog: 0,
    })))?;
    for line in io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let envelope = match Envelope::from_json(&line) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("invalid contract: {error}");
                continue;
            }
        };
        if let Message::AudioIngested(audio) = &envelope.message {
            if already_transcribed(audio, &lake_root) {
                continue;
            }
            match transcriber.transcribe(audio.clone()) {
                Ok(run) => emit(&Envelope::caused_by(
                    &envelope,
                    Message::TranscriptCompleted(run),
                ))?,
                Err(error) => emit(&Envelope::caused_by(
                    &envelope,
                    Message::Failure(Failure {
                        worker: WorkerRole::Transcribe,
                        code: "transcription_failed".into(),
                        detail: error.to_string(),
                        retryable: true,
                        failed_message_id: Some(envelope.id),
                    }),
                ))?,
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

    #[test]
    fn sidecar_backend_preserves_timestamps() {
        let temp = tempfile::tempdir().unwrap();
        let audio_path = temp.path().join("audio.wav");
        fs::write(&audio_path, b"audio").unwrap();
        fs::write(
            format!("{}.transcript.json", audio_path.display()),
            serde_json::json!({
                "model": "fixture",
                "model_version": "1",
                "segments": [{
                    "ordinal": 0,
                    "start_ms": 25,
                    "end_ms": 1250,
                    "speaker": null,
                    "text": "Remind me to test this tomorrow at 9am",
                    "confidence": 0.99,
                    "language": "en"
                }]
            })
            .to_string(),
        )
        .unwrap();
        unsafe { std::env::set_var("PERSONAL_AUDIO_LAKE_TRANSCRIBE_BACKEND", "sidecar") };
        let audio = AudioIngested {
            recording_id: Uuid::new_v4(),
            source_id: "fixture".into(),
            sha256: "a".repeat(64),
            relative_path: "audio.wav".into(),
            byte_len: 5,
            recorded_at_ms: 1,
            ingested_at_ms: 2,
        };
        let result = transcribe(&audio, temp.path()).unwrap();
        assert_eq!(result.segments[0].start_ms, 25);
        assert_eq!(result.segments[0].end_ms, 1250);
        unsafe { std::env::remove_var("PERSONAL_AUDIO_LAKE_TRANSCRIBE_BACKEND") };
    }

    #[test]
    fn immutable_transcript_object_prevents_duplicate_work() {
        let temp = tempfile::tempdir().unwrap();
        let audio = AudioIngested {
            recording_id: Uuid::new_v4(),
            source_id: "fixture".into(),
            sha256: "a".repeat(64),
            relative_path: "audio.wav".into(),
            byte_len: 5,
            recorded_at_ms: 1,
            ingested_at_ms: 2,
        };
        let directory = temp
            .path()
            .join("objects/transcripts")
            .join(audio.recording_id.to_string());
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("run.json"), b"{}").unwrap();
        assert!(already_transcribed(&audio, temp.path()));
    }
}
