# agy / Cloud Code protocol notes

What `paper-agy-switch` relies on when it reads quota. Captured from agy 1.4.4 (2026-10-22) by pointing
`CLOUD_CODE_URL` at a local recorder, then replaying the same calls with the same token.

## Hosts

| Host | Use |
|---|---|
| `https://daily-cloudcode-pa.googleapis.com` | what the agy CLI talks to |
| `https://cloudcode-pa.googleapis.com` (prod) | what the Antigravity IDE/desktop talks to |

agy reads the base from the `CLOUD_CODE_URL` environment variable when it is set. This tool honours
`PAPER_AGY_SWITCH_CLOUDCODE_URL` the same way, for both quota sources, and `PAPER_AGY_SWITCH_USER_AGENT`
overrides the user agent.

## Calls

All calls are `POST <base>/v1internal:<method>` with `Authorization: Bearer <access token>` and
`Content-Type: application/json`.

| Method | Body |
|---|---|
| `loadCodeAssist` | see below; the response carries `cloudaicompanionProject` and the tier |
| `fetchAvailableModels` | `{"project": <project>}`, or `{}` when there is no project |
| `retrieveUserQuotaSummary` | `{"project": <project>}`, or `{}` when there is no project |

### agy CLI (`[quota] source = "cli"`, default)

- User agent: `antigravity/cli/<agy version> (aidev_client; os_type=<windows|darwin|linux>; arch=<amd64|arm64>; cl=1003595037; auth_method=consumer)`.
  `<agy version>` comes from `agy --version` on PATH (cached per process); it falls back to `1.4.4`.
- `loadCodeAssist` body, exactly: `{"metadata":{"ideType":"ANTIGRAVITY"}}`.
- Daily host only. There is no fallback to prod, because prod would silently return the IDE quota.

### Antigravity IDE (`[quota] source = "ide"`)

- User agent: `antigravity/1.20.5 windows/amd64`.
- `loadCodeAssist` body: `{"metadata":{"ideName":"antigravity","ideType":"ANTIGRAVITY","ideVersion":"1.20.5","pluginVersion":"<this tool>","platform":"WINDOWS_AMD64","updateChannel":"stable","pluginType":"GEMINI"},"mode":"FULL_ELIGIBILITY_CHECK"}`.
- Prod host first, then daily on a network error, 404 or 5xx (a 429 stops at once).

## `retrieveUserQuotaSummary` response

```json
{"groups":[{"displayName":"Gemini","buckets":[
  {"bucketId":"...","window":"WEEKLY","remainingFraction":0.075,"resetTime":"2026-10-25T00:00:00Z","description":"..."}
]}]}
```

`remainingFraction` is 0..1 (the tool shows it as a percentage). Windows seen: weekly and 5-hour, per group.

## CLI quota differs from IDE quota

Same account, same token:

| Query | Gemini weekly | Gemini 5h | Claude/GPT weekly | Claude/GPT 5h |
|---|---|---|---|---|
| daily host + CLI user agent (matches `agy -p /usage`) | 0.075 | 0.419 | 0.252 | 1 |
| prod host, any user agent (IDE/desktop quota) | 0.982 | 1 | - | - |
| daily host + IDE user agent | Claude/GPT group only | | | |

So the host (and the CLI user agent on daily) decides which quota comes back. A tool that queries prod shows the
IDE numbers, not what the agy CLI will actually enforce.

## Credentials

- Windows Credential Manager target `gemini:antigravity` (generic credential) holds the login JSON.
  Switching accounts replaces that entry.
- agy uses a credential file instead of the keyring only when the keyring times out. This tool can read and write
  that file store only when `PAPER_AGY_SWITCH_CREDENTIAL_FILE` points at it; `doctor` names the store in use.
