# Contributing

Captain’s Log is published under AGPL-3.0-only with a separately negotiated commercial-licensing
path. Code and documentation contributions are submitted under the applicable
[Individual](CLA/individual.md) or [Entity](CLA/entity.md) Harmony Option Five Contributor License
Agreement. Contributors retain copyright and their accepted work remains available in the
AGPL-licensed project. By intentionally submitting a contribution, you confirm that you have read
and agree to the applicable CLA. If you submit on behalf of an entity, you represent that you are
authorised to bind that entity to the Entity CLA. Do not submit a contribution if you do not agree.

## Development setup

Use macOS 14 or newer with the pinned Rust toolchain, JDK 17, Android SDK 35, `jq`, Nix, Ollama, and
the standard Apple command-line tools. `tools/install-runtime.sh` installs or verifies the local
runtime dependencies; `tools/install-user.sh` builds and installs the per-user app.

Before proposing a change, run:

```sh
tools/verify-all.sh
```

Focused Rust and Android commands are documented in [README.md](README.md) and
[docs/android.md](docs/android.md).

## Architecture rules

- Keep the workspace dependency graph a strict tree rooted at
  `personal-audio-lake-contracts`.
- Put device protocols, operating-system APIs, databases, model runtimes, and UI frameworks in leaf
  adapters. Use root traits/generics in process and versioned messages between processes.
- Keep ViewModels renderer- and side-effect-independent. Views bind presentation values and emit
  semantic intents.
- A model and a person must be able to fill the same bounded workflow slot. Neither may change the
  state-machine topology or bypass deterministic validation.
- New sources must pass the shared source conformance contract. Pocket-specific behavior stays in
  its compatibility leaf; Android remains a BLE-only recorder source.

## Privacy and evidence

Use synthetic fixtures. Never commit or post audio, transcripts, event data, databases, tokens,
credentials, device identifiers, personal paths, generated packages, or Android
`local.properties`. Hardware claims must name the tested OS/device class without exposing a unique
identifier. Report vulnerabilities privately according to [SECURITY.md](SECURITY.md).

Update tests, relevant documentation, and [CHANGELOG.md](CHANGELOG.md) with behavioral changes.
Follow [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) in all project spaces.

## Third-party material

Submit only material whose copyright and patent rights you or your represented entity control. Do
not copy code, documentation, tests, generated output, protocol descriptions, or assets from
another project into a contribution merely because that project is open source. Propose third-party
material separately, identify its exact source and licence, and obtain maintainer approval before
including it. Mark communications that are not intended as contributions with `Not a Contribution`.
