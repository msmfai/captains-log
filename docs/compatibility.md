# Compatibility notice

Captain’s Log is a source-neutral local data lake and automation host.
Pocket is the first supported source adapter, not the identity or architectural
boundary of the application. Future adapters may ingest recordings or events
from other devices, Mac applications, folders, local services, or user-created
connectors. Destinations follow the same model: Calendar, Reminders, Notes,
Shortcuts, files, and webhooks are adapters over implementation-agnostic
workflow contracts.

## Independent compatibility implementation

Captain’s Log is independently developed and is not made, sponsored,
endorsed, or supported by Open Vision Engineering, Inc. “Pocket” is used only
to identify the third-party recorder with which this adapter interoperates.

## Pocket binding warning

Pocket pairing is application-level binding, not an ordinary removable entry in
the macOS Bluetooth device list. The recorder stores an application credential supplied
by the client. Once claimed, it may stop presenting the user-visible pairing
state until a client authenticates with that same key and releases the binding.

This creates a serious recovery requirement:

- Quitting or disconnecting the Mac does not release the recorder.
- Removing Pocket from the normal macOS Bluetooth list is not sufficient.
- Deleting ordinary application state does not clear the recorder's binding.
- Losing the matching credential can make the recorder appear bricked and may
  require vendor support.

Captain’s Log therefore stores the binding credential in macOS Keychain, independently of
ordinary application data. **Settings → Release source** authenticates with that stored credential,
requests a recorder-side binding
reset, disconnects, and clears the locally pinned device identifier.

The app deliberately retains the Keychain item after release. The recorder does
not provide a programmatically verifiable pairing-mode acknowledgement, so
automatically deleting the only recovery credential would be unsafe. Retention
also means reinstalling the same signed application can recover the key and retry
release if the first attempt was interrupted. Users should not remove the
Keychain item manually.

Before moving Pocket to another phone or computer, release it in the app and
check the recorder shows its normal pairing indication. If it does not, keep the
app and recovery key installed, wake the recorder, and retry. The compatibility
claim does not imply that macOS can recover a recorder after both its binding
state and the Mac Keychain credential have been lost.

## Compatibility claims

An adapter is supported only when its complete lifecycle has been tested:
initial connection, normal ingestion, reconnect, interruption recovery,
credential persistence, explicit disconnection, application reinstall, and
safe removal. Read and transfer success alone are not a sufficient compatibility
claim.

Adding another source must not introduce its protocol, credentials, or lifecycle
rules into the lakehouse core. The adapter emits the shared message contracts;
the lake, transcript pipeline, workflows, GUI, and integrations remain
implementation-agnostic.
