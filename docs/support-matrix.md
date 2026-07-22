# Support matrix

| Capability | macOS app/server | Android recorder |
|---|---:|---:|
| Record from local microphone | Compatibility/source adapters | Yes, foreground capture |
| Import an existing audio file | Yes | No in the first recorder release |
| Durable raw audio and retry | Persistent lake/source inbox | App-private queue + resumable BLE |
| Chronological library and review | Yes | No; intentionally Mac-owned |
| Transcription and local 24B routing | Yes | No; runs after Mac ingest |
| Workflows and native integrations | Yes | No; Mac-owned |
| Pocket discover/pair/transfer/release | macOS compatibility adapter | No |
| Runtime permissions | Capability-specific Mac popups | Microphone, then Nearby devices when sending |

Android’s implemented source path builds and is unit/framework tested. A physical-device release
test must still cover pairing, encrypted transfer, interruption/resume, Bluetooth-off recovery,
and multi-recording order. Pocket compatibility is independent of the Android recorder.
