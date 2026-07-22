# Privacy guarantees

- Audio, transcripts, model responses, manifests, credentials, and databases live outside the repo.
- The default server is loopback-only and requires a random bearer token.
- Transcription and semantic routing are local; no cloud model endpoint is implemented.
- Raw event and object data are canonical. ClickHouse projections are rebuildable and disposable.
- Source credentials belong to source adapters. The macOS Pocket key uses Keychain and survives
  ordinary uninstall so a recorder can still be released after reinstall.
- Permissions are requested at the moment a capability is used. The app does not send users to
  System Settings and continues with reduced capabilities after denial.
- Android uses app-private files and SQLite for recordings and pending-transfer state. Its only
  persisted transport identity is a random source UUID. Backup and device transfer exclude those
  stores. Uninstall removes recordings that have not yet been committed to the Mac.

The project does not claim anonymous telemetry because it implements no telemetry at all.

## Recording other people

Recording, transcription, and use of another person's speech may require notice
or consent depending on the people, place, and purpose involved. Users are
responsible for giving any required notice, obtaining any required consent, and
complying with the laws that apply to their recordings. Captain’s Log does
not upload recordings to a project-operated service or change those obligations.
