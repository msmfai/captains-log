#!/usr/bin/env bash
set -euo pipefail

root=personal-audio-lake-contracts
metadata="$(cargo metadata --format-version 1 --no-deps)"

workspace_names="$(jq -r '.packages[].name' <<<"$metadata" | sort)"
expected_names="$(jq -r '.root, .leaves[]' architecture.json | sort)"
if [ "$workspace_names" != "$expected_names" ]; then
  echo "workspace crates differ from architecture.json" >&2
  diff -u <(printf '%s\n' "$expected_names") <(printf '%s\n' "$workspace_names") >&2 || true
  exit 1
fi

while IFS= read -r package; do
  name="$(jq -r '.name' <<<"$package")"
  internal="$(jq -r --argjson names "$(jq '.packages | map(.name)' <<<"$metadata")" '
    [.dependencies[].name | select(. as $dep | $names | index($dep))] | unique | .[]?' <<<"$package")"
  if [ "$name" = "$root" ]; then
    if [ -n "$internal" ]; then
      echo "$root must not depend on another workspace crate: $internal" >&2
      exit 1
    fi
  elif [ "$internal" != "$root" ]; then
    echo "$name must have exactly one internal dependency ($root), got: ${internal:-none}" >&2
    exit 1
  fi
done < <(jq -c '.packages[]' <<<"$metadata")

view_model=crates/personal-audio-lake-app/src/view_model.rs
text_view=crates/personal-audio-lake-app/src/text_view.rs
native_view=crates/personal-audio-lake-app/src/menu_bar.rs
controller=crates/personal-audio-lake-app/src/app_controller.rs

if rg -n 'objc2|reqwest|std::fs|process::Command|app_controller|menu_bar|text_ui|text_view' "$view_model"; then
  echo "view_model.rs must remain renderer- and side-effect-independent" >&2
  exit 1
fi
if rg -n 'objc2|reqwest|std::fs|process::Command|app_controller|menu_bar|text_ui' "$text_view"; then
  echo "text_view.rs may only parse and render the view model" >&2
  exit 1
fi
if rg -n 'crate::read_model|read_model::|AppRecording|AppWorkflowRun|AppSnapshot' "$native_view"; then
  echo "menu_bar.rs may bind only view-model presentation values and intents, never model types" >&2
  exit 1
fi
intrinsic_content="$(sed -n '/fn render_recording_detail/,/fn prompt(/p' "$native_view")"
if rg -n 'setFrame|rect\(' <<<"$intrinsic_content"; then
  echo "Recording, Automations, and Settings content must use intrinsic arranged layout, never fixed coordinates" >&2
  exit 1
fi
if rg -n 'objc2|menu_bar|text_ui|text_view' "$controller"; then
  echo "app_controller.rs must not depend on a concrete view" >&2
  exit 1
fi
if rg -n 'x-apple\.systempreferences|open-notification-settings' crates; then
  echo "The app must never route a user into System Settings; permissions are popup-only" >&2
  exit 1
fi
if rg -n 'open-pocket|open_pocket|release_pocket|pocket_settings|/private/pocket' \
    crates/personal-audio-lake-app; then
  echo "generic desktop controls must not be named after a compatibility adapter" >&2
  exit 1
fi

android_view=android/app/src/main/java/org/personalaudiolake/app/MainActivity.java
android_view_model=android/app/src/main/java/org/personalaudiolake/app/MainViewModel.java
if rg -n '\.model\.|\.adapters\.|HttpURLConnection|JSONObject' "$android_view"; then
  echo "Android view may bind only ViewModel presentation values and intents" >&2
  exit 1
fi
if rg -n '^import android\.|android\.view|android\.widget|android\.app' "$android_view_model"; then
  echo "Android ViewModel must remain renderer- and lifecycle-independent" >&2
  exit 1
fi
android_model=android/app/src/main/java/org/personalaudiolake/app/model
android_adapters=android/app/src/main/java/org/personalaudiolake/app/adapters
android_source=android/app/src/main/java/org/personalaudiolake/app
if rg -n 'PocketSource|LibraryEntry|ReviewClient|ActionClient|EffectAdapter|Calendar' "$android_source"; then
  echo "Android is a recorder source; receiver, compatibility-device, and action code is forbidden" >&2
  exit 1
fi
for contract in 'AudioRecorder' 'PendingAudioStore<Input>' 'RecordingSender'; do
  if ! rg -F -q "$contract" android/app/src/main/java; then
    echo "Android generic contract is missing: $contract" >&2
    exit 1
  fi
done
if ! rg -q 'new AndroidAudioRecorder' android/app/src/main/java/org/personalaudiolake/app/AppComposition.java ||
   ! rg -q 'new BluetoothRecordingSender' android/app/src/main/java/org/personalaudiolake/app/AppComposition.java; then
  echo "Android composition must bind the recorder and encrypted Bluetooth sender leaves" >&2
  exit 1
fi
android_manifest=android/app/src/main/AndroidManifest.xml
if ! rg -q 'android.permission.RECORD_AUDIO' "$android_manifest" ||
   ! rg -q 'android.permission.BLUETOOTH_ADVERTISE' "$android_manifest" ||
   ! rg -q 'android.permission.BLUETOOTH_CONNECT' "$android_manifest" ||
   rg -q 'android.permission.INTERNET|CALENDAR|POST_NOTIFICATIONS' "$android_manifest"; then
  echo "Android must request only microphone and local Bluetooth transfer permissions" >&2
  exit 1
fi
if rg -n 'java\.net|android\.net|HttpURLConnection|URLConnection|OkHttp|Retrofit|WebSocket|WifiManager|ConnectivityManager|NetworkCapabilities' \
      android/app/src/main -g '*.java' -g '*.kt' ||
   rg -n 'okhttp|retrofit|ktor|volley|network_security_config' \
      android/app/build.gradle android/app/src/main; then
  echo "Android-to-Mac transfer is Bluetooth-only; network clients and network configuration are forbidden" >&2
  exit 1
fi
for uuid in \
  325ed7d9-14d2-4bae-ac92-ebf257933eb2 \
  325ed7da-14d2-4bae-ac92-ebf257933eb2 \
  325ed7db-14d2-4bae-ac92-ebf257933eb2 \
  325ed7dc-14d2-4bae-ac92-ebf257933eb2; do
  rust_uuid="${uuid//-/_}"
  if ! rg -qi "$uuid" android/app/src/main/java/org/personalaudiolake/app/adapters/BluetoothTransferProtocol.java ||
     ! rg -qi "$rust_uuid" crates/personal-audio-lake-bluetooth/src/main.rs; then
    echo "Bluetooth transfer contract UUID differs between Android and Mac: $uuid" >&2
    exit 1
  fi
done
if ! rg -q 'fn start_data_and_commit_match_the_android_compatibility_vectors' \
      crates/personal-audio-lake-bluetooth/src/main.rs ||
   ! rg -q 'start_data_and_commit_match_the_mac_compatibility_vectors' \
      android/app/src/test/java/org/personalaudiolake/app/adapters/BluetoothTransferProtocolTest.java ||
   ! rg -q 'BluetoothTransferProtocol\.parseStart' \
      android/app/src/main/java/org/personalaudiolake/app/adapters/BluetoothRecordingSender.java ||
   ! rg -q 'BluetoothTransferProtocol\.parseCommit' \
      android/app/src/main/java/org/personalaudiolake/app/adapters/BluetoothRecordingSender.java ||
   ! rg -q 'BluetoothTransferProtocol\.dataFrame' \
      android/app/src/main/java/org/personalaudiolake/app/adapters/BluetoothRecordingSender.java; then
  echo "Bluetooth manifest, start, data, and commit framing must share production codecs and cross-language vectors" >&2
  exit 1
fi
if ! rg -q 'PERMISSION_READ_ENCRYPTED_MITM' \
      android/app/src/main/java/org/personalaudiolake/app/adapters/BluetoothRecordingSender.java ||
   ! rg -q 'PERMISSION_WRITE_ENCRYPTED_MITM' \
      android/app/src/main/java/org/personalaudiolake/app/adapters/BluetoothRecordingSender.java; then
  echo "Android Bluetooth transfer characteristics must require authenticated encrypted access" >&2
  exit 1
fi

source_contract=crates/personal-audio-lake-contracts/src/lib.rs
if ! rg -q 'pub fn validate_source_contract\(' "$source_contract"; then
  echo "shared generic source conformance contract is missing" >&2
  exit 1
fi
if ! rg -q 'fn device_leaf_satisfies_the_shared_source_contract' \
    crates/personal-audio-lake-pocket/src/main.rs ||
   ! rg -q 'fn validate_source_envelope' crates/personal-audio-lake-pocket/src/main.rs ||
   ! rg -q 'validate_source_contract' crates/personal-audio-lake-pocket/src/main.rs; then
  echo "process-isolated device source must validate every production source envelope" >&2
  exit 1
fi
for producer in \
  'crates/personal-audio-lake-app/src/main.rs:FolderSourceAdapter:folder_leaf_satisfies_the_shared_source_contract' \
  'crates/personal-audio-lake-cli/src/main.rs:ApplicationFileSource:application_file_leaf_satisfies_the_shared_source_contract' \
  'crates/personal-audio-lake-app/src/api.rs:NetworkUploadSource:upload_uses_the_generic_recording_event_ingress'; do
  file="${producer%%:*}"
  remainder="${producer#*:}"
  adapter="${remainder%%:*}"
  test_name="${remainder#*:}"
  if ! rg -q "impl SourceAdapter<RecordingAvailable> for ${adapter}" "$file" ||
     ! rg -q 'execute_recording_source' "$file" ||
     ! rg -q "fn ${test_name}" "$file"; then
    echo "in-process source must use the generic production driver: $file ($adapter)" >&2
    exit 1
  fi
done
if ! rg -F -q 'impl<Writer: Write> MessageTransport<Envelope>' \
    crates/personal-audio-lake-app/src/main.rs ||
   ! rg -F -q 'impl EventStore<Envelope> for Lake' \
    crates/personal-audio-lake-storage/src/main.rs ||
   ! rg -F -q 'impl Transcriber<AudioIngested, TranscriptCompleted> for LocalTranscriber' \
    crates/personal-audio-lake-transcription/src/main.rs ||
   ! rg -F -q 'impl Oracle<ModelOracleContext, WorkflowSelection> for LocalModelOracle' \
    crates/personal-audio-lake-services/src/main.rs ||
   ! rg -F -q 'impl Oracle<(), WorkflowSelection> for HumanSelectionOracle' \
    crates/personal-audio-lake-services/src/main.rs ||
   ! rg -F -q 'impl EffectAdapter<ActionProposal, NativeEffectReceipt> for EventKitEffectAdapter' \
    crates/personal-audio-lake-macos-actions/src/main.rs ||
   ! rg -F -q 'impl ReadModelClient<(), AppSnapshot> for LocalApiReadModelClient' \
    crates/personal-audio-lake-app/src/read_model.rs ||
   ! rg -F -q 'impl ReadModelClient<Uuid, Vec<AppSegment>> for LocalApiReadModelClient' \
    crates/personal-audio-lake-app/src/read_model.rs; then
  echo "in-process transport, event-store, transcription, oracle, integration, and client relationships must implement root ports" >&2
  exit 1
fi

if ! rg -q 'fn device_source_enabled\(' crates/personal-audio-lake-app/src/main.rs ||
   ! rg -q 'tools/verify-non-pocket-ingest\.sh' tools/verify-all.sh; then
  echo "host ingress must remain operable and verified without a device compatibility adapter" >&2
  exit 1
fi
if ! rg -q 'validate_source_contract' crates/personal-audio-lake-bluetooth/src/main.rs ||
   ! rg -q 'fn android_bluetooth_leaf_satisfies_the_shared_source_contract' \
      crates/personal-audio-lake-bluetooth/src/main.rs; then
  echo "Android Bluetooth ingress must satisfy the same generic source contract" >&2
  exit 1
fi
if ! rg -q 'WorkerRole::TransferIngress' crates/personal-audio-lake-bluetooth/src/main.rs ||
   ! rg -q '\.workers \| length == 8' tools/install-user.sh ||
   ! rg -q 'fn should_start_transfer_ingress\(' \
      crates/personal-audio-lake-app/src/main.rs ||
   ! rg -q 'fn transfer_ingress_waits_for_source_adapter_readiness\(' \
      crates/personal-audio-lake-app/src/main.rs; then
  echo "local recording-transfer readiness must be explicit and installation-blocking" >&2
  exit 1
fi
if ! rg -q 'fn parse_worker_health\(' crates/personal-audio-lake-app/src/read_model.rs ||
   ! rg -q 'recording_transfer_state' crates/personal-audio-lake-app/src/view_model.rs ||
   ! rg -q 'settings_separate_always_on_transfer_from_optional_device_selection' \
      crates/personal-audio-lake-app/src/app_controller.rs; then
  echo "recorder-app transfer health and optional device selection must remain separate MVVM state" >&2
  exit 1
fi
if ! rg -q 'nix build --out-link "\$whisper_link"' tools/install-runtime.sh ||
   rg -q 'nix build --no-link.*whisper' tools/install-runtime.sh; then
  echo "Nix-backed runtime dependencies must retain a persistent garbage-collection root" >&2
  exit 1
fi
if ! rg -q 'ephemeral_worker_health_never_enters_the_immutable_user_lake' \
    crates/personal-audio-lake-app/src/main.rs ||
   ! rg -q 'ephemeral worker readiness leaked into the immutable user lake' \
    tools/verify-non-pocket-ingest.sh; then
  echo "worker readiness must remain outside the immutable personal-data lake" >&2
  exit 1
fi

release_surface=(crates docs README.md config.example.toml)
if rg -n \
  'DebugMatrix|debug_matrix|APP&D&|packet capture|firmware inspection|app inspection' \
  "${release_surface[@]}"; then
  echo "release surface contains destructive or diagnostic Bluetooth functionality" >&2
  exit 1
fi

echo "architecture: strict crate tree and view/view-model boundaries verified"
