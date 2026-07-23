# Local server API and services

The server binds to `api_bind` (default `127.0.0.1:32145`). It serves the GUI at
`/` and a versioned JSON API under `/api/v1`.

## Authentication

Opening `/` sets an HttpOnly, SameSite=Strict local cookie. Other clients read
`~/Library/Application Support/PersonalAudioLake/api-token`, created with mode 0600,
and send `Authorization: Bearer TOKEN`.

## API surface

- `GET /api/v1/health`
- `GET /api/v1/inbox`
- `POST /api/v1/inbox/RECORDING_UUID/review`
- `GET /api/v1/recordings?q=TEXT`
- `GET /api/v1/recordings/RECORDING_UUID`
- `GET /api/v1/audio/RECORDING_UUID`
- `POST /api/v1/ingest/audio`
- `GET /api/v1/actions`
- `POST /api/v1/actions/ACTION_UUID/approve`
- `POST /api/v1/actions/ACTION_UUID/reject`
- `POST /api/v1/actions/ACTION_UUID/edit`
- `POST /api/v1/actions/ACTION_UUID/delivery`
- `POST /api/v1/actions/ACTION_UUID/external-undo`
- `POST /api/v1/annotations`
- `POST /api/v1/integrations/macos/request-calendar-access`
- `POST /api/v1/integrations/macos/request-reminders-access`
- `GET /api/v1/integrations/macos/notifications`
- `POST /api/v1/integrations/macos/request-notification-access`
- `GET /api/v1/events?after=CURSOR&limit=100`
- `GET|POST /api/v1/services`
- `PUT|DELETE /api/v1/services/SERVICE_ID`
- `GET|PUT /api/v1/rules`

`POST /api/v1/ingest/audio` accepts a raw body up to 256 MiB with a supported audio
`Content-Type`. `X-Source-Id` is an optional generic source identity and `X-Recorded-At-Ms` is an
optional positive Unix millisecond timestamp. The server chooses the file name, writes the body
atomically into its private source inbox, and queues `RecordingAvailable`; clients cannot supply a
path. Authentication is identical to every other `/api/v1` route. Chunked transfer and arbitrary
binary content types are rejected.

`GET /api/v1/health` returns `ready` only when storage and the current-generation source,
recording-transfer ingress, lake, transcription, derivation, Calendar, notification, and
workflow-service workers have each emitted typed `WorkerHealth { state: ready }`. The transfer
ingress reports ready only after the local transport initializes and reports degraded on bounded
scan or transfer failures. The transcription worker verifies its executable, ffmpeg,
and non-empty local model file. Every LLM leaf independently inspects the configured Ollama model
and applies the shared exact-24B contract. Missing or degraded workers are returned explicitly in
the `workers` array; stale readiness from an earlier process is cleared at startup.

External clients report a completed semantic effect only after the platform side effect succeeds:

```json
{"target":"external.calendar","external_id":"42","delivered_at_ms":1800000000000}
```

`POST .../delivery` queues the versioned `ActionDelivered` contract. After compensating the effect,
the same client posts `{"target":"external.calendar","undone_at_ms":1800000001000}` to
`.../external-undo`, which queues `ActionUndone`. Target and external identities are bounded,
non-empty, control-free strings. The endpoints do not contain Android or macOS behavior; platform
leaves own that mapping and retain the external identity needed for undo.

The event endpoint is replayable. Consumers persist the returned cursor only
after their side effect succeeds.

## User-owned workflows

The GUI creates versioned state-machine definitions such as:

```json
{
  "id": "archive-transcripts",
  "name": "Archive completed transcripts",
  "enabled": true,
  "subscriptions": ["transcript_completed"],
  "initial_state_id": "message_ingested",
  "states": [
    {
      "id": "message_ingested",
      "description": "A new recording arrived",
      "selector": {"type": "deterministic", "transition_id": "archive"},
      "transitions": [
        {
          "id": "archive",
          "description": "Archive the transcript",
          "destination_state_id": "done",
          "output_fields": [],
          "action": {
            "type": "effect",
            "adapter": {"type": "shortcut", "target": "Archive Recording Transcript"}
          }
        }
      ]
    },
    {
      "id": "done",
      "description": "Complete",
      "selector": {"type": "terminal", "outcome": "complete"},
      "transitions": []
    }
  ]
}
```

Effect adapters include reversible Calendar/Reminders actions, Notes, saved Mail
drafts, Contacts, text files, named Shortcuts, and webhooks. Webhooks require
HTTPS except for explicit loopback URLs. Shortcuts receive structured workflow
input as a JSON file through `shortcuts run`. A workflow may declare a matching
undo adapter. Each workflow has an independent checkpoint and durable run trace;
failures do not advance its cursor. New workflows start at the live manifest
cursor and do not retroactively act on old recordings.

## Determinism and the local model

Explicit and hybrid rules use Rust first for normalized fuzzy trigger matching,
payload extraction, time resolution, identity, policy, and delivery. A cheap
keyword prefilter may send command-like near misses to the model as constrained
rule candidates; enabled implicit rules use the same judge. Before use, each
worker verifies through the local Ollama API that it is a 24B model. The default
is the already-installed `mistral-small3.2:latest`. Enabled rules execute
unambiguous decisions immediately; Review is acknowledgement and correction,
not an approval gate. The primary default Mac integrations are user-space OSM
workflows rather than these legacy rules. No cloud model endpoint is implemented.
