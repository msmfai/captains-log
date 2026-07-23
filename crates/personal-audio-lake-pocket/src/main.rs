use std::collections::BTreeSet;
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use btleplug::api::{
    Central, CharPropFlags, Characteristic, Manager as _, Peripheral as _, ScanFilter, WriteType,
};
use btleplug::platform::{Adapter, Manager, Peripheral};
use chrono::{NaiveDateTime, Utc};
use clap::{Parser, Subcommand};
use futures::StreamExt;
use personal_audio_lake_contracts::{
    Envelope, Failure, HealthState, Message, RecordingAvailable, SourceAdapterOutput,
    SourceCapability, SourceCommand, SourceOperation, SourceState, SourceStatus, WorkerHealth,
    WorkerRole, validate_source_contract,
};
use security_framework::passwords::{get_generic_password, set_generic_password};
use tokio::fs::{self, File};
use tokio::io::AsyncWriteExt;
use tokio::time::{sleep, timeout};
use uuid::Uuid;

const ADAPTER_ID: &str = "pocket";
// Functional interface identifiers used only by the Pocket interoperability leaf.
const SERVICE_UUID: Uuid = Uuid::from_u128(0x001120a0_2233_4455_6677_889912345678);
const DATA_UUID: Uuid = Uuid::from_u128(0x001120a1_2233_4455_6677_889912345678);
const CONTROL_UUID: Uuid = Uuid::from_u128(0x001120a2_2233_4455_6677_889912345678);
const STATUS_UUID: Uuid = Uuid::from_u128(0x001120a3_2233_4455_6677_889912345678);
const BINDING_KEY_SERVICE: &str = "com.smfmarin.personal-audio-lake.pocket-binding";
const BINDING_KEY_ACCOUNT: &str = "default";

#[derive(Parser)]
#[command(about = "Pocket Bluetooth recording sync")]
struct Cli {
    #[arg(long, default_value = "Pocket")]
    expected_name: String,
    #[arg(long)]
    approved_device_id: Option<String>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Discover a compatible recorder and emit generic source status.
    Discover {
        #[arg(long, default_value_t = 12_000)]
        duration_ms: u64,
    },
    /// Accept versioned NDJSON commands on stdin and emit results on stdout.
    Worker,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Worker) {
        Command::Discover { duration_ms } => {
            let adapter = adapter().await?;
            let request = Envelope::root(Message::SourceCommandRequested(SourceCommand {
                adapter_id: ADAPTER_ID.into(),
                source_id: cli.approved_device_id,
                operation: SourceOperation::Discover {
                    duration_ms,
                    allow_pairing: true,
                },
            }));
            for result in active_scan(&adapter, &request).await {
                emit(&result?)?;
            }
        }
        Command::Worker => worker().await?,
    }
    Ok(())
}

async fn adapter() -> Result<Adapter> {
    let manager = timeout(Duration::from_secs(10), Manager::new())
        .await
        .context("timed out opening CoreBluetooth manager")?
        .context("open CoreBluetooth manager")?;
    timeout(Duration::from_secs(10), manager.adapters())
        .await
        .context("timed out listing CoreBluetooth adapters")??
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("no Bluetooth Low Energy adapter available"))
}

async fn start_scan(adapter: &Adapter) -> Result<()> {
    timeout(
        Duration::from_secs(5),
        adapter.start_scan(ScanFilter::default()),
    )
    .await
    .context("timed out starting CoreBluetooth scan")?
    .context("start CoreBluetooth scan")
}

async fn worker() -> Result<()> {
    let mut reported_wait = false;
    let adapter = loop {
        match adapter().await {
            Ok(adapter) => break adapter,
            Err(error) => {
                if !reported_wait {
                    emit(&Envelope::root(Message::WorkerHealth(WorkerHealth {
                        role: WorkerRole::Source,
                        state: HealthState::Degraded,
                        detail: format!("Bluetooth adapter is not ready; retrying: {error:#}"),
                        backlog: 0,
                    })))?;
                    reported_wait = true;
                }
                sleep(Duration::from_secs(1)).await;
            }
        }
    };
    emit(&Envelope::root(Message::WorkerHealth(WorkerHealth {
        role: WorkerRole::Source,
        state: HealthState::Ready,
        detail: "Bluetooth source adapter ready".into(),
        backlog: 0,
    })))?;
    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let request = Envelope::from_json(&line)?;
        let results = match source_operation(&request) {
            Ok(SourceOperation::Discover { .. }) => active_scan(&adapter, &request).await,
            Ok(SourceOperation::Sync { .. }) => match process_transfer(&adapter, &request).await {
                Ok(()) => Vec::new(),
                Err(error) => vec![Err(error)],
            },
            Ok(SourceOperation::Release) => match release_device(&adapter, &request).await {
                Ok(result) => vec![Ok(result)],
                Err(error) => vec![Err(error)],
            },
            Err(error) => vec![Ok(failure(
                &request,
                "unsupported_message",
                &error.to_string(),
                false,
            ))],
        };
        for result in results {
            emit(&result.unwrap_or_else(|error| {
                failure(&request, "bluetooth_error", &format!("{error:#}"), true)
            }))?;
        }
    }
    Ok(())
}

fn source_command(envelope: &Envelope) -> Result<&SourceCommand> {
    let Message::SourceCommandRequested(command) = &envelope.message else {
        bail!("source adapter accepts only source commands");
    };
    if command.adapter_id != ADAPTER_ID {
        bail!("source command targets adapter {}", command.adapter_id);
    }
    Ok(command)
}

fn source_operation(envelope: &Envelope) -> Result<&SourceOperation> {
    Ok(&source_command(envelope)?.operation)
}

async fn release_device(adapter: &Adapter, envelope: &Envelope) -> Result<Envelope> {
    let request = source_command(envelope)?;
    if !matches!(request.operation, SourceOperation::Release) {
        bail!("expected source release request");
    }
    let source_id = request
        .source_id
        .as_deref()
        .context("release requires a source id")?;
    let binding_key = binding_key()?;
    start_scan(adapter).await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let peripheral = loop {
        if let Some(value) = find_peripheral(adapter, source_id).await? {
            break value;
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("Pocket {source_id} was not discovered for release");
        }
        sleep(Duration::from_millis(250)).await;
    };
    connect(&peripheral).await?;
    let characteristics = peripheral.characteristics();
    let control = find_characteristic(
        &characteristics,
        SERVICE_UUID,
        CONTROL_UUID,
        CharPropFlags::WRITE | CharPropFlags::WRITE_WITHOUT_RESPONSE,
    )?;
    let status = find_characteristic(
        &characteristics,
        SERVICE_UUID,
        STATUS_UUID,
        CharPropFlags::NOTIFY | CharPropFlags::INDICATE,
    )?;
    timeout(Duration::from_secs(5), peripheral.subscribe(&status))
        .await
        .context("timed out subscribing to Pocket release status")??;
    let mut notifications = timeout(Duration::from_secs(5), peripheral.notifications())
        .await
        .context("timed out opening Pocket release notifications")??;
    timeout(
        Duration::from_secs(5),
        peripheral.write(
            &control,
            format!("APP&SK&{binding_key}").as_bytes(),
            WriteType::WithoutResponse,
        ),
    )
    .await
    .context("timed out authenticating Pocket release")??;
    let response = collect_status_until(
        &mut notifications,
        status.uuid,
        b"MCU&SK&OK",
        Duration::from_secs(4),
    )
    .await?;
    if !contains_bytes(&response, b"MCU&SK&OK") {
        bail!("Pocket rejected the stored binding key; release was not attempted");
    }
    timeout(
        Duration::from_secs(5),
        peripheral.write(&control, b"APP&BLE&RESET", WriteType::WithoutResponse),
    )
    .await
    .context("timed out requesting Pocket release")??;
    sleep(Duration::from_secs(1)).await;
    let _ = timeout(Duration::from_secs(5), peripheral.disconnect()).await;
    Ok(Envelope::caused_by(
        envelope,
        Message::SourceStatusChanged(SourceStatus {
            adapter_id: ADAPTER_ID.into(),
            source_id: source_id.into(),
            display_name: "Pocket".into(),
            state: SourceState::Released,
            capabilities: capabilities(),
            attributes: Default::default(),
        }),
    ))
}

fn validate_binding_key(value: &str) -> Result<()> {
    if value.len() != 16 || !value.is_ascii() {
        bail!("Pocket binding key must be exactly 16 ASCII characters");
    }
    Ok(())
}

fn binding_key() -> Result<String> {
    let value = match get_generic_password(BINDING_KEY_SERVICE, BINDING_KEY_ACCOUNT) {
        Ok(bytes) => String::from_utf8(bytes).context("Pocket binding key is not UTF-8")?,
        Err(error) if error.code() == -25300 => {
            let value = Uuid::new_v4().simple().to_string()[..16].to_owned();
            set_generic_password(BINDING_KEY_SERVICE, BINDING_KEY_ACCOUNT, value.as_bytes())
                .context("store Pocket binding key in macOS Keychain")?;
            value
        }
        Err(error) => return Err(error).context("read Pocket binding key from macOS Keychain"),
    };
    validate_binding_key(&value)?;
    Ok(value)
}

fn capabilities() -> Vec<SourceCapability> {
    vec![
        SourceCapability::Discover,
        SourceCapability::Pair,
        SourceCapability::Sync,
        SourceCapability::Release,
        SourceCapability::BackgroundIngest,
    ]
}

async fn process_transfer(adapter: &Adapter, envelope: &Envelope) -> Result<()> {
    if !matches!(source_operation(envelope)?, SourceOperation::Sync { .. }) {
        bail!("expected source sync request");
    };
    active_pocket_sync(adapter, envelope).await
}

async fn active_scan(adapter: &Adapter, envelope: &Envelope) -> Vec<Result<Envelope>> {
    let Ok(request) = source_command(envelope) else {
        return vec![Err(anyhow!("expected source command"))];
    };
    let SourceOperation::Discover {
        duration_ms,
        allow_pairing,
    } = request.operation
    else {
        return vec![Err(anyhow!("expected discover operation"))];
    };
    if let Err(error) = start_scan(adapter).await {
        return vec![Err(error)];
    }
    sleep(Duration::from_millis(duration_ms)).await;
    let mut output = Vec::new();
    let peripherals = match adapter.peripherals().await {
        Ok(value) => value,
        Err(error) => return vec![Err(error.into())],
    };
    for peripheral in peripherals {
        match observe_and_probe(&peripheral, envelope, request, allow_pairing).await {
            Ok(values) => output.extend(values.into_iter().map(Ok)),
            Err(error) => output.push(Err(error)),
        }
    }
    if output.is_empty() {
        output.push(Ok(failure(
            envelope,
            "no_peripherals",
            "CoreBluetooth scan completed without discovering the configured device",
            true,
        )));
    }
    output
}

async fn observe_and_probe(
    peripheral: &Peripheral,
    envelope: &Envelope,
    request: &SourceCommand,
    allow_pairing: bool,
) -> Result<Vec<Envelope>> {
    let properties = peripheral.properties().await?.unwrap_or_default();
    let device_id = format!("{}", peripheral.id());
    let name = properties.local_name.or(properties.advertisement_name);
    let approved = request
        .source_id
        .as_ref()
        .is_some_and(|approved| approved == &device_id);
    let name_match = name
        .as_deref()
        .is_some_and(|name| recognized_product_name(name, "Pocket"));
    let recognized = approved || (request.source_id.is_none() && allow_pairing && name_match);
    if !recognized {
        return Ok(Vec::new());
    }
    let mut attributes = std::collections::BTreeMap::new();
    if let Some(rssi) = properties.rssi {
        attributes.insert("signal_dbm".into(), rssi.to_string());
    }
    let display_name = name.unwrap_or_else(|| "Pocket".into());
    connect(peripheral).await?;
    Ok(vec![source_status(
        envelope,
        &device_id,
        &display_name,
        SourceState::Ready,
        attributes,
    )])
}

fn source_status(
    envelope: &Envelope,
    source_id: &str,
    display_name: &str,
    state: SourceState,
    attributes: std::collections::BTreeMap<String, String>,
) -> Envelope {
    Envelope::caused_by(
        envelope,
        Message::SourceStatusChanged(SourceStatus {
            adapter_id: ADAPTER_ID.into(),
            source_id: source_id.into(),
            display_name: display_name.into(),
            state,
            capabilities: capabilities(),
            attributes,
        }),
    )
}

fn recognized_product_name(advertised_name: &str, configured_name: &str) -> bool {
    let advertised_name = advertised_name.to_ascii_lowercase();
    advertised_name.contains(&configured_name.to_ascii_lowercase())
        || advertised_name.starts_with("pkt01_")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PocketFile {
    folder: String,
    name: String,
}

async fn active_pocket_sync(adapter: &Adapter, envelope: &Envelope) -> Result<()> {
    let request = source_command(envelope)?;
    let SourceOperation::Sync {
        destination_uri,
        idle_timeout_ms,
    } = &request.operation
    else {
        bail!("expected source sync request");
    };
    let source_id = request
        .source_id
        .as_deref()
        .context("sync requires a source id")?;
    let binding_key = binding_key()?;
    let destination_dir = destination_uri
        .strip_prefix("file://")
        .unwrap_or(destination_uri);

    start_scan(adapter).await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let peripheral = loop {
        if let Some(value) = find_peripheral(adapter, source_id).await? {
            break value;
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("Pocket {source_id} was not discovered");
        }
        sleep(Duration::from_millis(250)).await;
    };

    connect(&peripheral).await?;
    let mut characteristics = peripheral.characteristics();
    let mut control = find_characteristic(
        &characteristics,
        SERVICE_UUID,
        CONTROL_UUID,
        CharPropFlags::WRITE | CharPropFlags::WRITE_WITHOUT_RESPONSE,
    )?;
    let mut data = find_characteristic(
        &characteristics,
        SERVICE_UUID,
        DATA_UUID,
        CharPropFlags::NOTIFY | CharPropFlags::INDICATE,
    )?;
    let mut status = find_characteristic(
        &characteristics,
        SERVICE_UUID,
        STATUS_UUID,
        CharPropFlags::NOTIFY | CharPropFlags::INDICATE,
    )?;
    peripheral.subscribe(&data).await?;
    if data.uuid != status.uuid {
        peripheral.subscribe(&status).await?;
    }
    let mut notifications = peripheral.notifications().await?;

    peripheral
        .write(
            &control,
            format!("APP&SK&{binding_key}").as_bytes(),
            WriteType::WithoutResponse,
        )
        .await?;
    let first_binding = collect_status_until(
        &mut notifications,
        status.uuid,
        b"MCU&SK&OK",
        Duration::from_secs(3),
    )
    .await;
    let bound = match first_binding {
        Ok(bytes) if contains_bytes(&bytes, b"MCU&SK&OK") => true,
        Ok(bytes) if contains_bytes(&bytes, b"MCU&SK&ERR") => false,
        _ => {
            peripheral.disconnect().await.ok();
            sleep(Duration::from_secs(1)).await;
            connect(&peripheral).await?;
            characteristics = peripheral.characteristics();
            control = find_characteristic(
                &characteristics,
                SERVICE_UUID,
                CONTROL_UUID,
                CharPropFlags::WRITE | CharPropFlags::WRITE_WITHOUT_RESPONSE,
            )?;
            data = find_characteristic(
                &characteristics,
                SERVICE_UUID,
                DATA_UUID,
                CharPropFlags::NOTIFY | CharPropFlags::INDICATE,
            )?;
            status = find_characteristic(
                &characteristics,
                SERVICE_UUID,
                STATUS_UUID,
                CharPropFlags::NOTIFY | CharPropFlags::INDICATE,
            )?;
            peripheral.subscribe(&data).await?;
            if data.uuid != status.uuid {
                peripheral.subscribe(&status).await?;
            }
            notifications = peripheral.notifications().await?;
            peripheral
                .write(
                    &control,
                    format!("APP&SK&{binding_key}").as_bytes(),
                    WriteType::WithoutResponse,
                )
                .await?;
            let bytes = collect_status_until(
                &mut notifications,
                status.uuid,
                b"MCU&SK&OK",
                Duration::from_secs(4),
            )
            .await?;
            contains_bytes(&bytes, b"MCU&SK&OK")
        }
    };
    if !bound {
        bail!("Pocket rejected this Mac's binding key; put the recorder in pairing mode");
    }

    emit(&Envelope::caused_by(
        envelope,
        Message::SourceStatusChanged(SourceStatus {
            adapter_id: ADAPTER_ID.into(),
            source_id: source_id.into(),
            display_name: "Pocket".into(),
            state: SourceState::Bound,
            capabilities: capabilities(),
            attributes: Default::default(),
        }),
    ))?;

    let directory_response = send_status_command(
        &peripheral,
        &control,
        &mut notifications,
        status.uuid,
        b"APP&LIST_DIRS",
        b"MCU&DIRS_SUM&",
    )
    .await?;
    let directories = parse_prefixed_fields(&directory_response, "MCU&DIRS&");
    let mut files = Vec::new();
    for folder in directories {
        let response = send_status_command(
            &peripheral,
            &control,
            &mut notifications,
            status.uuid,
            format!("APP&LIST&{folder}").as_bytes(),
            b"MCU&LIST&",
        )
        .await?;
        for record in parse_prefixed_fields(&response, "MCU&F&") {
            let mut fields = record.split('&');
            if let (Some(folder), Some(name), Some(_duration)) =
                (fields.next(), fields.next(), fields.next())
            {
                files.push(PocketFile {
                    folder: folder.to_owned(),
                    name: name.to_owned(),
                });
            }
        }
    }

    fs::create_dir_all(destination_dir).await?;
    for remote in files {
        let basename = format!(
            "{}-{}.mp3",
            safe_component(&remote.folder),
            safe_component(&remote.name)
        );
        let final_path = Path::new(destination_dir).join(basename);
        let announced_path = final_path.with_extension("mp3.announced");
        if final_path.exists() {
            if !announced_path.exists() {
                emit(&recording_envelope(
                    envelope,
                    source_id,
                    &remote,
                    &final_path,
                ))?;
                fs::write(&announced_path, envelope.id.to_string()).await?;
            }
            continue;
        }

        let partial_path = final_path.with_extension("mp3.partial");
        let mut file = File::create(&partial_path).await?;
        peripheral
            .write(
                &control,
                format!("APP&U&{}&{}", remote.folder, remote.name).as_bytes(),
                WriteType::WithoutResponse,
            )
            .await?;
        let mut status_bytes = Vec::new();
        let mut received = 0_u64;
        let transfer_result: Result<()> = async {
            loop {
                match timeout(
                    Duration::from_millis((*idle_timeout_ms).max(1_000)),
                    notifications.next(),
                )
                .await
                {
                    Ok(Some(notification)) if notification.uuid == data.uuid => {
                        file.write_all(&notification.value).await?;
                        received += notification.value.len() as u64;
                    }
                    Ok(Some(notification)) if notification.uuid == status.uuid => {
                        status_bytes.extend_from_slice(&notification.value);
                        if contains_bytes(&status_bytes, b"MCU&U&ERR") {
                            bail!("Pocket rejected download of {}", remote.name);
                        }
                        if contains_bytes(&status_bytes, b"MCU&OFF") {
                            break;
                        }
                    }
                    Ok(Some(_)) => {}
                    Ok(None) => bail!("Pocket disconnected while downloading {}", remote.name),
                    Err(_) => bail!("Pocket download timed out for {}", remote.name),
                }
            }
            let expected = parse_download_size(&status_bytes)
                .ok_or_else(|| anyhow!("Pocket omitted download size for {}", remote.name))?;
            if received != expected {
                bail!(
                    "Pocket download size mismatch for {}: received {received}, expected {expected}",
                    remote.name
                );
            }
            file.flush().await?;
            file.sync_all().await?;
            Ok(())
        }
        .await;
        if let Err(error) = transfer_result {
            drop(file);
            fs::remove_file(&partial_path).await.ok();
            return Err(error);
        }
        drop(file);
        fs::rename(&partial_path, &final_path).await?;
        emit(&recording_envelope(
            envelope,
            source_id,
            &remote,
            &final_path,
        ))?;
        fs::write(&announced_path, envelope.id.to_string()).await?;
    }

    peripheral.unsubscribe(&data).await.ok();
    if data.uuid != status.uuid {
        peripheral.unsubscribe(&status).await.ok();
    }
    Ok(())
}

fn recording_envelope(
    envelope: &Envelope,
    source_id: &str,
    remote: &PocketFile,
    final_path: &Path,
) -> Envelope {
    Envelope::caused_by(
        envelope,
        Message::RecordingAvailable(recording_available(source_id, remote, final_path)),
    )
}

fn recording_available(
    source_id: &str,
    remote: &PocketFile,
    final_path: &Path,
) -> RecordingAvailable {
    let recorded_at_ms = NaiveDateTime::parse_from_str(&remote.name, "%Y%m%d%H%M%S")
        .map(|value| value.and_utc().timestamp_millis())
        .unwrap_or_else(|_| Utc::now().timestamp_millis());
    RecordingAvailable {
        source_id: source_id.into(),
        adapter_id: ADAPTER_ID.into(),
        source_path: final_path.display().to_string(),
        recorded_at_ms,
    }
}

async fn send_status_command(
    peripheral: &Peripheral,
    control: &Characteristic,
    notifications: &mut (impl futures::Stream<Item = btleplug::api::ValueNotification> + Unpin),
    status_uuid: Uuid,
    command: &[u8],
    terminal: &[u8],
) -> Result<Vec<u8>> {
    peripheral
        .write(control, command, WriteType::WithoutResponse)
        .await?;
    collect_status_until(notifications, status_uuid, terminal, Duration::from_secs(5)).await
}

async fn collect_status_until(
    notifications: &mut (impl futures::Stream<Item = btleplug::api::ValueNotification> + Unpin),
    status_uuid: Uuid,
    terminal: &[u8],
    idle: Duration,
) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    loop {
        match timeout(idle, notifications.next()).await {
            Ok(Some(notification)) if notification.uuid == status_uuid => {
                output.extend_from_slice(&notification.value);
                if contains_bytes(&output, terminal) || contains_bytes(&output, b"MCU&SK&ERR") {
                    return Ok(output);
                }
            }
            Ok(Some(_)) => {}
            Ok(None) => bail!("Pocket disconnected while waiting for status"),
            Err(_) => bail!("Pocket status timed out"),
        }
    }
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|value| value == needle)
}

fn parse_prefixed_fields(input: &[u8], prefix: &str) -> Vec<String> {
    let text = String::from_utf8_lossy(input);
    text.split("MCU&")
        .filter_map(|record| record.strip_prefix(prefix.trim_start_matches("MCU&")))
        .map(str::to_owned)
        .collect()
}

fn parse_download_size(input: &[u8]) -> Option<u64> {
    let text = String::from_utf8_lossy(input);
    let tail = text.split("MCU&U&").nth(1)?;
    let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

fn safe_component(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

async fn connect(peripheral: &Peripheral) -> Result<()> {
    let connected = timeout(Duration::from_secs(5), peripheral.is_connected())
        .await
        .context("timed out checking Pocket connection state")??;
    if !connected {
        timeout(Duration::from_secs(10), peripheral.connect())
            .await
            .context("timed out connecting to Pocket")?
            .context("connect to Pocket")?;
    }
    timeout(Duration::from_secs(10), peripheral.discover_services())
        .await
        .context("timed out discovering Pocket services")?
        .context("discover Pocket services")?;
    Ok(())
}

async fn find_peripheral(adapter: &Adapter, device_id: &str) -> Result<Option<Peripheral>> {
    let peripherals = timeout(Duration::from_secs(5), adapter.peripherals())
        .await
        .context("timed out listing discovered Bluetooth peripherals")??;
    for peripheral in peripherals {
        if format!("{}", peripheral.id()) == device_id {
            return Ok(Some(peripheral));
        }
    }
    Ok(None)
}

fn find_characteristic(
    characteristics: &BTreeSet<Characteristic>,
    service_uuid: Uuid,
    uuid: Uuid,
    allowed: CharPropFlags,
) -> Result<Characteristic> {
    let value = characteristics
        .iter()
        .find(|value| value.service_uuid == service_uuid && value.uuid == uuid)
        .cloned()
        .ok_or_else(|| anyhow!("GATT characteristic {uuid} not found in service {service_uuid}"))?;
    if !value.properties.intersects(allowed) {
        bail!(
            "GATT characteristic {uuid} has incompatible properties {:?}",
            value.properties
        );
    }
    Ok(value)
}

fn failure(parent: &Envelope, code: &str, detail: &str, retryable: bool) -> Envelope {
    Envelope::caused_by(
        parent,
        Message::Failure(Failure {
            worker: WorkerRole::Source,
            code: code.to_owned(),
            detail: detail.to_owned(),
            retryable,
            failed_message_id: Some(parent.id),
        }),
    )
}

fn emit(envelope: &Envelope) -> Result<()> {
    validate_source_envelope(envelope).map_err(anyhow::Error::msg)?;
    let mut stdout = io::stdout().lock();
    stdout.write_all(envelope.to_ndjson()?.as_bytes())?;
    stdout.flush()?;
    Ok(())
}

fn validate_source_envelope(envelope: &Envelope) -> std::result::Result<(), String> {
    match &envelope.message {
        Message::SourceStatusChanged(status) => validate_source_contract(
            &status.adapter_id,
            &status.source_id,
            &[SourceAdapterOutput::Status(status.clone())],
        ),
        Message::RecordingAvailable(recording) => validate_source_contract(
            &recording.adapter_id,
            &recording.source_id,
            &[SourceAdapterOutput::Artifact(recording.clone())],
        ),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_fixtures_are_supported() {
        personal_audio_lake_contracts::assert_contract_fixtures();
    }

    #[test]
    fn device_leaf_satisfies_the_shared_source_contract() {
        let source_id = "fixture-recorder";
        let cause = Envelope::root(Message::SourceCommandRequested(SourceCommand {
            adapter_id: ADAPTER_ID.into(),
            source_id: Some(source_id.into()),
            operation: SourceOperation::Release,
        }));
        let status = source_status(
            &cause,
            source_id,
            "Recorder",
            SourceState::Released,
            Default::default(),
        );
        let Message::SourceStatusChanged(status) = status.message else {
            panic!("source status helper emitted the wrong contract message");
        };
        let artifact = recording_available(
            source_id,
            &PocketFile {
                folder: "fixture".into(),
                name: "19700101000001".into(),
            },
            Path::new("/private/source-inbox/recording.mp3"),
        );
        personal_audio_lake_contracts::validate_source_contract(
            ADAPTER_ID,
            source_id,
            &[
                personal_audio_lake_contracts::SourceAdapterOutput::Status(status.clone()),
                personal_audio_lake_contracts::SourceAdapterOutput::Artifact(artifact.clone()),
            ],
        )
        .unwrap();
        validate_source_envelope(&Envelope::root(Message::SourceStatusChanged(status))).unwrap();
        validate_source_envelope(&Envelope::root(Message::RecordingAvailable(artifact))).unwrap();
    }

    #[test]
    fn recognizes_the_recorder_product_name_used_in_ble_advertising() {
        assert!(recognized_product_name("PKT01_GREY_EXAMPLE", "Pocket"));
        assert!(recognized_product_name("My Pocket", "Pocket"));
        assert!(!recognized_product_name("Nearby Headphones", "Pocket"));
    }

    #[test]
    fn parses_directory_file_and_download_status() {
        assert_eq!(
            parse_prefixed_fields(b"MCU&DIRS&2026-07-13MCU&DIRS_SUM&001", "MCU&DIRS&"),
            vec!["2026-07-13"]
        );
        assert_eq!(
            parse_prefixed_fields(b"MCU&F&2026-07-13&20260713175316&2MCU&LIST&001", "MCU&F&"),
            vec!["2026-07-13&20260713175316&2"]
        );
        assert_eq!(parse_download_size(b"MCU&U&11274MCU&OFF"), Some(11_274));
        assert_eq!(safe_component("../2026/07/13"), "___2026_07_13");
    }

    #[test]
    fn release_requires_a_valid_recovery_key() {
        assert!(validate_binding_key("0123456789abcdef").is_ok());
        assert!(validate_binding_key("too-short").is_err());
    }
}
