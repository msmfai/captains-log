# Security policy

## Supported versions

Security fixes target the latest published release. Pre-release builds and old artifacts are not
supported.

| Version | Supported |
|---|---:|
| Latest release | Yes |
| Older or development builds | No |

## Reporting a vulnerability

Do not open a public issue for a vulnerability. In the public GitHub repository, open **Security →
Advisories → Report a vulnerability**. Source publication and binary releases are blocked until
GitHub private vulnerability reporting is enabled and maintainer notifications are verified, so the
report and any follow-up remain private to the reporter and repository maintainers.

Do not attach recordings, transcripts, databases, manifests, API tokens, Keychain material,
Bluetooth identifiers, or unredacted logs. A minimal description, affected version, impact, and
reproduction using synthetic data are sufficient for an initial report.

## Security boundaries

- The built-in API binds to loopback by default and uses a random bearer token stored with mode
  `0600`. It is not an internet-facing service. Anyone deliberately exposing it must provide their
  own authenticated encrypted transport.
- Audio, transcripts, model requests, and model responses remain local. No telemetry or cloud-model
  endpoint is implemented.
- Pocket recovery credentials stay in macOS Keychain and survive ordinary uninstall so a bound
  recorder can still be released after reinstall.
- Android recordings and transfer state use app-private storage excluded from backup. Android has no
  Internet permission or network fallback.

General bug reports belong in the issue tracker after removing all private data. See
[CONTRIBUTING.md](CONTRIBUTING.md).
