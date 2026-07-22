# Data lake and lakehouse

## Immutable data lake

The filesystem under `lake/` is the source of truth:

```text
lake/
├── manifests/{events.ndjson,sequence}
└── objects/
    ├── audio/<sha-prefix>/<sha256>.<extension>
    └── transcripts/<recording-id>/<run-id>.json
```

Audio is copied atomically and addressed by SHA-256. A transcript object contains
the recording time, model identity, timestamped normalized segments, and the
backend's exact structured payload. Objects are never edited. A correction emits
a new annotation tied to the recording, run, and segment.

Worker readiness is deliberately excluded. It belongs to the bounded `status/events.ndjson`
process journal, which is reset at supervisor startup so stale readiness cannot satisfy setup.
Operational restarts therefore cannot inflate or semantically contaminate the personal-event lake.

## Rebuildable lakehouse

ClickHouse is a query projection, not the durable source. It contains ingest
events, audio objects, transcript segments and annotations, action facts and
reviews, native/service delivery receipts, notification audit receipts, and
recording-level audit acknowledgements.

`personal-audio-lake-storage rebuild` truncates these projections and replays the immutable event
manifest. Content objects are retained. Stable UUIDv5 identities and receipts
make replay idempotent.

`notification_deliveries` stores the model-written recording summary alongside
machine-written hook states and targets. It is an audit projection; the original
transcript, action facts, reviews, and delivery receipts remain authoritative.

`recording_reviews` projects the latest `RecordingReviewed` message. `confirmed`
means the user checked the complete processing report; `needs_attention` retains
their mismatch note. No notification delivery receipt is treated as proof of
human review.

The GUI overlays the newest annotation while retaining the raw text and audio
time range. Every action carries its original quote and segment range.
