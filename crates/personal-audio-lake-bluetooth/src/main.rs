use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use btleplug::api::{Central, CharPropFlags, Manager as _, Peripheral as _, ScanFilter, WriteType};
use btleplug::platform::{Adapter, Manager, Peripheral};
use clap::Parser;
use futures::StreamExt;
use personal_audio_lake_contracts::{
    Envelope, HealthState, Message, RecordingAvailable, SourceAdapterOutput, WorkerHealth,
    WorkerRole, validate_source_contract,
};
use sha2::{Digest, Sha256};
use tokio::time::{sleep, timeout};
use uuid::Uuid;

const SERVICE_UUID: Uuid = Uuid::from_u128(0x325ed7d9_14d2_4bae_ac92_ebf257933eb2);
const CONTROL_UUID: Uuid = Uuid::from_u128(0x325ed7da_14d2_4bae_ac92_ebf257933eb2);
const MANIFEST_UUID: Uuid = Uuid::from_u128(0x325ed7db_14d2_4bae_ac92_ebf257933eb2);
const DATA_UUID: Uuid = Uuid::from_u128(0x325ed7dc_14d2_4bae_ac92_ebf257933eb2);
const MANIFEST_LEN: usize = 85;
const MAX_RECORDING_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const BLUETOOTH_OPERATION_TIMEOUT: Duration = Duration::from_secs(10);
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(10 * 60 + 5);

#[derive(Parser)]
#[command(about = "Encrypted Bluetooth recording receiver")]
struct Cli {
    #[arg(long, env = "PERSONAL_AUDIO_LAKE_STATE_ROOT")]
    state_root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Manifest {
    device_id: Uuid,
    recording_id: Uuid,
    size: u64,
    recorded_at_ms: i64,
    sha256: [u8; 32],
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut waiting_reported = false;
    let adapter = loop {
        match adapter().await {
            Ok(adapter) => break adapter,
            Err(error) => {
                if !waiting_reported {
                    emit_health(
                        HealthState::Degraded,
                        format!("local recording transfer unavailable; retrying: {error:#}"),
                    )?;
                    waiting_reported = true;
                }
                sleep(Duration::from_secs(1)).await;
            }
        }
    };
    emit_health(
        HealthState::Ready,
        "encrypted local recording transfer ready".into(),
    )?;
    let mut last_error = String::new();
    let mut ready = true;
    loop {
        match receive_once(&adapter, &cli.state_root).await {
            Ok(()) => {
                if !ready {
                    emit_health(
                        HealthState::Ready,
                        "encrypted local recording transfer ready".into(),
                    )?;
                }
                ready = true;
                last_error.clear();
            }
            Err(error) => {
                let detail = format!("{error:#}");
                if detail.contains("no Captain’s Log Android recorder is advertising") {
                    if !ready {
                        emit_health(
                            HealthState::Ready,
                            "encrypted local recording transfer ready".into(),
                        )?;
                    }
                    ready = true;
                    last_error.clear();
                } else {
                    if ready {
                        emit_health(
                            HealthState::Degraded,
                            format!("local recording transfer unavailable; retrying: {detail}"),
                        )?;
                    }
                    if detail != last_error {
                        eprintln!("phone Bluetooth transfer waiting: {detail}");
                    }
                    ready = false;
                    last_error = detail;
                }
            }
        }
        sleep(Duration::from_secs(2)).await;
    }
}

fn emit_health(state: HealthState, detail: String) -> Result<()> {
    emit(&Envelope::root(Message::WorkerHealth(WorkerHealth {
        role: WorkerRole::TransferIngress,
        state,
        detail,
        backlog: 0,
    })))
}

async fn adapter() -> Result<Adapter> {
    let manager = timeout(Duration::from_secs(10), Manager::new())
        .await
        .context("timed out opening Bluetooth manager")??;
    timeout(Duration::from_secs(10), manager.adapters())
        .await
        .context("timed out listing Bluetooth adapters")??
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("no Bluetooth Low Energy adapter available"))
}

async fn receive_once(adapter: &Adapter, state_root: &Path) -> Result<()> {
    let scan_started: Result<()> = match timeout(
        BLUETOOTH_OPERATION_TIMEOUT,
        adapter.start_scan(ScanFilter {
            services: vec![SERVICE_UUID],
        }),
    )
    .await
    {
        Ok(result) => result.map_err(Into::into),
        Err(error) => {
            Err(anyhow!(error)).context("timed out starting local recording-transfer scan")
        }
    };
    if let Err(error) = scan_started {
        timeout(BLUETOOTH_OPERATION_TIMEOUT, adapter.stop_scan())
            .await
            .ok();
        return Err(error);
    }
    sleep(Duration::from_secs(2)).await;
    let discovery = async {
        let mut candidate = None;
        let peripherals = timeout(BLUETOOTH_OPERATION_TIMEOUT, adapter.peripherals())
            .await
            .context("timed out listing local recording sources")??;
        for peripheral in peripherals {
            let properties = timeout(BLUETOOTH_OPERATION_TIMEOUT, peripheral.properties())
                .await
                .context("timed out reading local recording-source properties")??;
            if properties.is_some_and(|value| value.services.contains(&SERVICE_UUID)) {
                candidate = Some(peripheral);
                break;
            }
        }
        Result::<Option<Peripheral>>::Ok(candidate)
    }
    .await;
    timeout(BLUETOOTH_OPERATION_TIMEOUT, adapter.stop_scan())
        .await
        .ok();
    let candidate = discovery?;
    transfer(
        &candidate.context("no Captain’s Log Android recorder is advertising")?,
        state_root,
    )
    .await
}

async fn transfer(peripheral: &Peripheral, state_root: &Path) -> Result<()> {
    let result = match timeout(TRANSFER_TIMEOUT, transfer_connected(peripheral, state_root)).await {
        Ok(result) => result,
        Err(error) => {
            Err(anyhow!(error)).context("local recording transfer exceeded its ten-minute deadline")
        }
    };
    timeout(BLUETOOTH_OPERATION_TIMEOUT, peripheral.disconnect())
        .await
        .ok();
    result
}

async fn transfer_connected(peripheral: &Peripheral, state_root: &Path) -> Result<()> {
    if !peripheral.is_connected().await? {
        peripheral.connect().await?;
    }
    peripheral.discover_services().await?;
    let characteristics = peripheral.characteristics();
    let control = characteristics
        .iter()
        .find(|value| {
            value.uuid == CONTROL_UUID
                && value
                    .properties
                    .intersects(CharPropFlags::WRITE | CharPropFlags::WRITE_WITHOUT_RESPONSE)
        })
        .cloned()
        .context("Bluetooth control characteristic is missing")?;
    let manifest_characteristic = characteristics
        .iter()
        .find(|value| value.uuid == MANIFEST_UUID && value.properties.contains(CharPropFlags::READ))
        .cloned()
        .context("Bluetooth manifest characteristic is missing")?;
    let data = characteristics
        .iter()
        .find(|value| {
            value.uuid == DATA_UUID
                && value
                    .properties
                    .intersects(CharPropFlags::NOTIFY | CharPropFlags::INDICATE)
        })
        .cloned()
        .context("Bluetooth data characteristic is missing")?;
    let manifest = parse_manifest(&peripheral.read(&manifest_characteristic).await?)?;
    let partials = state_root.join("source-inbox/bluetooth-partials");
    let inbox = state_root.join("source-inbox");
    fs::create_dir_all(&partials)?;
    fs::create_dir_all(&inbox)?;
    let partial = partials.join(format!("{}.partial", manifest.recording_id));
    let (mut output, mut offset) = prepare_partial(&partial, &manifest)?;
    peripheral.subscribe(&data).await?;
    let mut notifications = peripheral.notifications().await?;
    peripheral
        .write(
            &control,
            &start_command(manifest.recording_id, offset),
            WriteType::WithResponse,
        )
        .await?;
    while offset < manifest.size {
        let notification = timeout(Duration::from_secs(20), notifications.next())
            .await
            .context("Bluetooth transfer became idle")?
            .context("Bluetooth notification stream ended")?;
        if notification.uuid != DATA_UUID {
            continue;
        }
        let Frame {
            offset: frame_offset,
            bytes,
        } = parse_frame(&notification.value)?;
        append_frame(
            &mut output,
            &mut offset,
            Frame {
                offset: frame_offset,
                bytes,
            },
            manifest.size,
        )?;
    }
    output.flush()?;
    output.sync_all()?;
    drop(output);
    verify_partial(&partial, &manifest)?;
    let destination = inbox.join(format!("android-{}.m4a", manifest.recording_id));
    commit_partial(&partial, &destination, &manifest)?;
    let recording = RecordingAvailable {
        source_id: format!("android:{}", manifest.device_id),
        adapter_id: "android-bluetooth".into(),
        source_path: destination.display().to_string(),
        recorded_at_ms: manifest.recorded_at_ms,
    };
    validate_recording(&recording)?;
    emit(&Envelope::root(Message::RecordingAvailable(recording)))?;
    peripheral
        .write(
            &control,
            &commit_command(manifest.recording_id, manifest.sha256),
            WriteType::WithResponse,
        )
        .await?;
    peripheral.unsubscribe(&data).await.ok();
    Ok(())
}

fn parse_manifest(value: &[u8]) -> Result<Manifest> {
    if value.len() != MANIFEST_LEN || &value[..4] != b"CLB1" || value[4] != 1 {
        bail!("unsupported Bluetooth manifest");
    }
    let manifest = Manifest {
        device_id: Uuid::from_slice(&value[5..21])?,
        recording_id: Uuid::from_slice(&value[21..37])?,
        size: u64::from_be_bytes(value[37..45].try_into()?),
        recorded_at_ms: i64::from_be_bytes(value[45..53].try_into()?),
        sha256: value[53..85].try_into()?,
    };
    if manifest.size == 0 || manifest.size > MAX_RECORDING_BYTES || manifest.recorded_at_ms <= 0 {
        bail!("invalid Bluetooth recording metadata");
    }
    Ok(manifest)
}

struct Frame<'a> {
    offset: u64,
    bytes: &'a [u8],
}
fn parse_frame(value: &[u8]) -> Result<Frame<'_>> {
    match value.first().copied() {
        Some(1) if value.len() > 9 => Ok(Frame {
            offset: u64::from_be_bytes(value[1..9].try_into()?),
            bytes: &value[9..],
        }),
        _ => bail!("invalid Bluetooth data frame"),
    }
}
fn prepare_partial(partial: &Path, manifest: &Manifest) -> Result<(File, u64)> {
    let mut offset = fs::metadata(partial).map(|value| value.len()).unwrap_or(0);
    if offset > manifest.size {
        fs::remove_file(partial)?;
        offset = 0;
    }
    let output = OpenOptions::new().create(true).append(true).open(partial)?;
    Ok((output, offset))
}
fn append_frame(
    output: &mut impl Write,
    offset: &mut u64,
    frame: Frame<'_>,
    announced_size: u64,
) -> Result<()> {
    if frame.offset != *offset {
        bail!(
            "Bluetooth frame gap at {}, received {}",
            *offset,
            frame.offset
        );
    }
    let next = offset
        .checked_add(frame.bytes.len() as u64)
        .context("Bluetooth transfer offset overflow")?;
    if next > announced_size {
        bail!("Bluetooth transfer exceeded announced size");
    }
    output.write_all(frame.bytes)?;
    *offset = next;
    Ok(())
}
fn start_command(id: Uuid, offset: u64) -> Vec<u8> {
    let mut v = vec![1];
    v.extend(id.as_bytes());
    v.extend(offset.to_be_bytes());
    v
}
fn commit_command(id: Uuid, hash: [u8; 32]) -> Vec<u8> {
    let mut v = vec![2];
    v.extend(id.as_bytes());
    v.extend(hash);
    v
}
fn hash_file(path: &Path) -> Result<[u8; 32]> {
    let mut input = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(digest.finalize().into())
}
fn verify_partial(partial: &Path, manifest: &Manifest) -> Result<()> {
    if hash_file(partial)? == manifest.sha256 {
        return Ok(());
    }
    fs::remove_file(partial)
        .with_context(|| format!("remove corrupt Bluetooth partial {}", partial.display()))?;
    bail!("Bluetooth recording failed SHA-256 verification; retrying from byte zero")
}
fn commit_partial(partial: &Path, destination: &Path, manifest: &Manifest) -> Result<()> {
    if !destination.exists() {
        fs::rename(partial, destination)?;
        return Ok(());
    }
    let metadata = fs::metadata(destination)?;
    if metadata.len() != manifest.size || hash_file(destination)? != manifest.sha256 {
        bail!("an existing recording conflicts with the Bluetooth manifest");
    }
    fs::remove_file(partial)?;
    Ok(())
}
fn validate_recording(recording: &RecordingAvailable) -> Result<()> {
    validate_source_contract(
        &recording.adapter_id,
        &recording.source_id,
        &[SourceAdapterOutput::Artifact(recording.clone())],
    )
    .map_err(anyhow::Error::msg)
}
fn emit(envelope: &Envelope) -> Result<()> {
    let mut out = io::stdout().lock();
    out.write_all(envelope.to_ndjson()?.as_bytes())?;
    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_hex(value: &[u8]) -> String {
        value.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn decode_hex(value: &str) -> Vec<u8> {
        (0..value.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&value[index..index + 2], 16).unwrap())
            .collect()
    }

    fn manifest(bytes: &[u8]) -> Manifest {
        Manifest {
            device_id: Uuid::new_v4(),
            recording_id: Uuid::new_v4(),
            size: bytes.len() as u64,
            recorded_at_ms: 10,
            sha256: Sha256::digest(bytes).into(),
        }
    }

    #[test]
    fn protocol_manifest_and_frames_are_strict() {
        let device = Uuid::new_v4();
        let recording = Uuid::new_v4();
        let hash = [7; 32];
        let mut bytes = b"CLB1".to_vec();
        bytes.push(1);
        bytes.extend(device.as_bytes());
        bytes.extend(recording.as_bytes());
        bytes.extend(9u64.to_be_bytes());
        bytes.extend(10i64.to_be_bytes());
        bytes.extend(hash);
        assert_eq!(
            parse_manifest(&bytes).unwrap(),
            Manifest {
                device_id: device,
                recording_id: recording,
                size: 9,
                recorded_at_ms: 10,
                sha256: hash
            }
        );
        let mut frame = vec![1];
        frame.extend(3u64.to_be_bytes());
        frame.extend(b"abc");
        assert!(matches!(
            parse_frame(&frame).unwrap(),
            Frame {
                offset: 3,
                bytes: b"abc"
            }
        ));
        assert!(parse_frame(b"bad").is_err());
        bytes[37..45].copy_from_slice(&(MAX_RECORDING_BYTES + 1).to_be_bytes());
        assert!(parse_manifest(&bytes).is_err());
    }

    #[test]
    fn manifest_matches_the_android_compatibility_vector() {
        let vector = "434c423101\
            00112233445566778899aabbccddeeff\
            102132435465768798a9bacbdcedfe0f\
            0000000000000005\
            0000018bcfe5687b\
            000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
        let bytes = decode_hex(vector);

        assert_eq!(
            parse_manifest(&bytes).unwrap(),
            Manifest {
                device_id: Uuid::parse_str("00112233-4455-6677-8899-aabbccddeeff").unwrap(),
                recording_id: Uuid::parse_str("10213243-5465-7687-98a9-bacbdcedfe0f").unwrap(),
                size: 5,
                recorded_at_ms: 1_700_000_000_123,
                sha256: std::array::from_fn(|index| index as u8),
            }
        );
    }

    #[test]
    fn start_data_and_commit_match_the_android_compatibility_vectors() {
        let recording = Uuid::parse_str("10213243-5465-7687-98a9-bacbdcedfe0f").unwrap();
        let hash = std::array::from_fn(|index| index as u8);

        assert_eq!(
            encode_hex(&start_command(recording, 258)),
            "01102132435465768798a9bacbdcedfe0f0000000000000102"
        );
        assert_eq!(
            encode_hex(&commit_command(recording, hash)),
            concat!(
                "02102132435465768798a9bacbdcedfe0f",
                "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"
            )
        );
        let frame = decode_hex("010000000000000102616263");
        assert!(matches!(
            parse_frame(&frame).unwrap(),
            Frame {
                offset: 258,
                bytes: b"abc"
            }
        ));
    }

    #[test]
    fn corrupt_complete_partial_is_removed_so_retry_starts_from_zero() {
        let temp = tempfile::tempdir().unwrap();
        let partial = temp.path().join("recording.partial");
        fs::write(&partial, b"corrupt").unwrap();
        let expected = manifest(b"correct");

        assert!(verify_partial(&partial, &expected).is_err());
        assert!(!partial.exists());
    }

    #[test]
    fn interrupted_partial_resumes_from_its_exact_offset() {
        let temp = tempfile::tempdir().unwrap();
        let partial = temp.path().join("recording.partial");
        let destination = temp.path().join("recording.m4a");
        let expected = manifest(b"audio");
        fs::write(&partial, b"au").unwrap();

        let (mut output, mut offset) = prepare_partial(&partial, &expected).unwrap();
        assert_eq!(offset, 2);
        append_frame(
            &mut output,
            &mut offset,
            Frame {
                offset: 2,
                bytes: b"dio",
            },
            expected.size,
        )
        .unwrap();
        output.sync_all().unwrap();
        drop(output);
        assert_eq!(offset, expected.size);
        verify_partial(&partial, &expected).unwrap();
        commit_partial(&partial, &destination, &expected).unwrap();
        assert_eq!(fs::read(destination).unwrap(), b"audio");
    }

    #[test]
    fn out_of_order_frame_is_rejected_without_appending_bytes() {
        let mut output = Vec::new();
        let mut offset = 2;
        let error = append_frame(
            &mut output,
            &mut offset,
            Frame {
                offset: 3,
                bytes: b"x",
            },
            5,
        )
        .unwrap_err();
        assert!(error.to_string().contains("frame gap"));
        assert!(output.is_empty());
        assert_eq!(offset, 2);
    }

    #[test]
    fn commit_is_idempotent_after_a_lost_phone_acknowledgement() {
        let temp = tempfile::tempdir().unwrap();
        let partial = temp.path().join("recording.partial");
        let destination = temp.path().join("recording.m4a");
        let expected = manifest(b"audio");

        fs::write(&partial, b"audio").unwrap();
        verify_partial(&partial, &expected).unwrap();
        commit_partial(&partial, &destination, &expected).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"audio");

        fs::write(&partial, b"audio").unwrap();
        verify_partial(&partial, &expected).unwrap();
        commit_partial(&partial, &destination, &expected).unwrap();
        assert!(!partial.exists());
        assert_eq!(fs::read(&destination).unwrap(), b"audio");
    }

    #[test]
    fn android_bluetooth_leaf_satisfies_the_shared_source_contract() {
        let recording = RecordingAvailable {
            source_id: "android:00112233-4455-6677-8899-aabbccddeeff".into(),
            adapter_id: "android-bluetooth".into(),
            source_path: "/private/source-inbox/android-recording.m4a".into(),
            recorded_at_ms: 1_700_000_000_123,
        };
        validate_recording(&recording).unwrap();

        let mut invalid = recording;
        invalid.source_id.clear();
        assert!(validate_recording(&invalid).is_err());
    }
}
