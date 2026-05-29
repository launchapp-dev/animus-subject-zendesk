# animus-subject-zendesk

Zendesk Support ticket subject backend for [Animus](https://github.com/launchapp-dev/animus-cli).

## What this is

`animus-subject-zendesk` exposes Zendesk Support tickets as Animus subjects so
workflows can triage, assign, tag, comment on, and solve customer support work
from the same protocol used by other Animus subject backends.

The plugin implements the current Animus subject backend protocol:

- `subject/list`
- `subject/get`
- `subject/update`
- `subject/schema`
- `health/check`

## Configuration

| Env var | Default | Purpose |
| --- | --- | --- |
| `ZENDESK_SUBDOMAIN` | unset | Builds `https://<subdomain>.zendesk.com` when `ZENDESK_BASE_URL` is unset. |
| `ZENDESK_BASE_URL` | unset | Zendesk account URL, for example `https://example.zendesk.com`. |
| `ZENDESK_EMAIL` | unset | Zendesk agent email used with API token authentication. |
| `ZENDESK_API_TOKEN` | unset | Zendesk API token. |
| `ZENDESK_QUERY` | unset | Optional Zendesk search query for `subject/list`. |

## Subject IDs

IDs use the shape:

```text
zendesk:<ticket-id>
```

Example:

```text
zendesk:12345
```

## Install

```bash
animus plugin install launchapp-dev/animus-subject-zendesk
```

## Smoke Test

```bash
cargo build --release
./target/release/animus-subject-zendesk --manifest
```

## License

MIT.
