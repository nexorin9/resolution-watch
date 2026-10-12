# Hospital Executive Meeting Resolution Tracker

> Drop last week's executive meeting minutes markdown into `inbox/2026-W41/minutes.md` every Monday morning, run `reso run --week 2026-W41`, and within 10 minutes you get a 6-file resolution tracking package in `out/2026-W41/` (with an audit chain, ready for sign-off, 6-bucket remaining-days counts readable at a glance).

Every Monday after the executive meeting ends, the secretary's notebook becomes unusable — dozens of resolutions span more than ten departments with deadlines scattered across four weeks, and every follow-up requires cross-checking PPT files and chat logs. This tool does not act as an "AI judge". It only handles: meeting minutes → structured resolution items → 5-state machine progression → 6-bucket remaining-days classification → 6-file export package with sha256 + 5-segment audit chain → secretary/department-head sign-off → OA-side progression, **this workflow runs as a long-lived local process**. The LLM is only used at the resolution extraction step; if schema validation fails, the system immediately falls back to a deterministic path that splits by section and uses regex extraction.

## Applicable Scenarios / Target Roles

| Role | When to Use | What You Get |
|------|-------------|--------------|
| President's Office Secretary / Supervisor | Monday morning 09:00–10:00, organize last week's executive meeting minutes | `out/<week_id>/` resolution tracking ledger with 4 status buckets (pending / in_progress / due_soon / overdue) and 6-bucket remaining-day counts |
| President's Office Director / Vice President | Daily check of near-deadline and overdue resolutions for follow-up and coordination | `out/<week_id>/overdue.jsonl` + `due_soon.jsonl` lists, aggregated by responsible department, ready to be sent via DingTalk/email directly to the department head |
| Information Systems Engineer | Monday morning 09:00–10:00 batch CLI launch, monitor 5-segment audit chain | `audit_log.jsonl` + sha256 manifest, OA outbound call logs, complete LLM call metadata buckets |
| Department Director / Section Head | Mid-week receives departmental resolution list (with deadlines and status), proceeds according to established actions | CSV segments printable as a sign-off sheet, every resolution entry has all 5 required fields filled |

> **Table Discipline**: This table only lists role vocabulary + business timing + verifiable outputs; REST paths, HL7 fields, status enums, webhook parameters all go into "Commands / API / Configuration".

## Key Capabilities

- **One Monday batch run delivers a 6-file export package**: Run `reso run --week 2026-W41` and get `resolutions.csv` + `overdue.jsonl` + `due_soon.jsonl` + `conflicts.jsonl` + `summary.json` + `audit_log.jsonl` + `sha256.txt` in `out/2026-W41/`, signable, sign-off sheet printable.
- **Schema-first structured extraction of resolution items**: Meeting minutes md go through LLM extraction + 5 required-field schema validation (any one missing is rejected), then a deterministic fallback that splits by `## Resolution N:` sections; when the LLM is unreachable, the system explicitly degrades rather than silently modifying data.
- **5-state machine rejects illegal transitions**: Resolution status only advances within the sequence `pending → in_progress → due_soon → overdue → completed`; any OA event attempting to skip intermediate states (e.g. pending → completed without an in_progress event) returns an `InvalidTransition` error and writes to `conflicts.jsonl`, never silently altering the database.
- **6-bucket remaining-days derivation**: Remaining days fall into six buckets `[0,3]` / `(3,7]` / `(7,14]` / `(14,30]` / `>30` / `<0` (including overdue bucket negative-value judgment), `summary.json` gives a weekly rhythm at a glance.
- **Sqlite week-level idempotency**: Running `reso run --week <same week>` a second time does not overwrite the first resolution progression (`upsert_idempotent` + `week_idempotency` dual protection), resolutions from different weeks don't cross-pollinate.
- **OA and LLM credentials always read from environment variables**: bearer tokens, connection strings, and model keys are never written into code or tests; the local `.env.example` is purely illustrative.

## Quick Start

### Dependencies

- Rust toolchain (rustc ≥ 1.75, cargo ≥ 1.75): [rustup.rs](https://rustup.rs/)
- OA endpoint reachable (HTTP mode) or OA database read-only account (SQL mode)
- LLM endpoint: OpenAI-compatible (`/v1/chat/completions`, such as OpenAI / Azure / vLLM / Ollama `/v1` compatible)
- Optional: DingTalk/WeCom webhook (for overdue notifications)

### Installation

```bash
# 1) After pulling the code, enter the product subdirectory
cd resolution-watch

# 2) Copy the environment variable sample and fill in actual values
cp .env.example .env
# Edit .env and fill in at least OA_API_BEARER / OA_QUERY_DSN / MODEL_API_KEY

# 3) Build (includes rusqlite bundled feature, no system Sqlite needed)
cargo build --release
```

### Configuration

| Config Item | Default | Description |
|-------------|---------|-------------|
| `OA_API_BEARER` | (required) | OA REST bearer token, read from deployment environment variables |
| `OA_QUERY_DSN` | (required) | HTTP mode: `https://oa.example.com/api/v1`; SQL mode: `postgres://…` |
| `RESO_DB_PATH` | `./.reso_cache.sqlite` | Local cache Sqlite path |
| `RESO_MINUTES_DIR` | `./inbox` | Meeting minutes drop location, structure `<DIR>/<week_id>/minutes.md` |
| `OUT_DIR` | `./out` | Export package output directory |
| `MODEL_BASE_URL` | (required) | OpenAI-compatible endpoint base_url |
| `MODEL_API_KEY` | (required) | LLM endpoint API key |
| `MODEL_NAME` | (required) | Model name (e.g. `gpt-4o-mini`) |
| `MODEL_TIMEOUT_MS` | `15000` | Single LLM request timeout (milliseconds) |
| `WECOM_WEBHOOK_URL` | (empty) | Optional DingTalk/WeCom notification webhook; absent means no notification |

### Build & Verify

```bash
# Run all unit tests (including end-to-end mock integration)
cargo test --manifest-path resolution-watch/Cargo.toml

# Run mockito end-to-end (OA + LLM real stub layer) simulating a given week
cargo test --manifest-path resolution-watch/Cargo.toml --test integration_official

# Validate outputs (example)
test -d out/2026-W41 && ls out/2026-W41/   # Should see 7 files (6 files + sha256)
```

### Command Cheat Sheet

```bash
# Weekly batch: extract last week's executive meeting resolutions + progress + export 6 files
reso run --week 2026-W41

# Only ingest meeting minutes (no resolution extraction, no progression)
reso ingest --week 2026-W41

# Only verify the export package + sha256 (does not touch OA)
reso verify --week 2026-W41

# Mark a sign-off (does not affect resolution state progression)
reso sign --week 2026-W41 --signer-id 10086
```

## Commands / API / Configuration

### Key Configuration Items

| Config Item | Type | Default | Description |
|-------------|------|---------|-------------|
| `OA_API_BEARER` | env | — | OA REST/SQL outbound credentials |
| `OA_QUERY_DSN` | env | — | OA endpoint descriptor |
| `RESO_DB_PATH` | env | `./.reso_cache.sqlite` | Local Sqlite cache |
| `RESO_MINUTES_DIR` | env | `./inbox` | Meeting minutes entry point |
| `OUT_DIR` | env | `./out` | Export package output |
| `MODEL_BASE_URL` | env | — | LLM endpoint base_url |
| `MODEL_API_KEY` | env | — | LLM endpoint API key |
| `MODEL_NAME` | env | — | Model name |
| `MODEL_TIMEOUT_MS` | env (int) | `15000` | Single LLM timeout |
| `WECOM_WEBHOOK_URL` | env | — | Optional notification webhook |

### CLI Subcommands / API Endpoints

| Command or Path | Purpose | Key Parameters |
|-----------------|--------|----------------|
| `reso run` | Weekly batch main entry | `--week` (required, ISO week, e.g. `2026-W41`) |
| `reso ingest` | Only ingest meeting minutes, no resolution extraction | `--week` (required) |
| `reso verify` | Verify export package + sha256 | `--week` (required) |
| `reso sign` | Mark a sign-off check-in | `--week` (required) + `--signer-id` (required) |
| `OA GET /oa/events` | Pull workflow events for that week | `?week_id=2026-W41&cursor=xxx` |
| `OA POST /oa/events` | (Placeholder: this tool does not write back to OA; after secretary sign-off, the OA side proceeds per established actions) | — |
| `LLM POST /v1/chat/completions` | Resolution extraction (system + user) | OpenAI-compatible JSON request |

### Errors & Degradation

| Scenario | Behavior | Handling |
|----------|----------|----------|
| Any env variable missing | Immediate error on startup | Exit code 1, stderr prints missing key |
| OA 5xx / connection error | Exponential backoff retry 5 times (500ms→8s) | Still fails with exit code 2 |
| OA 4xx | Immediate failure, no retry | Exit code 2, stderr prints response body |
| LLM timeout (15000ms) | Retry once + still failing falls to deterministic_fallback | Exit code 0 (degraded), audit records `ai_degraded=true` |
| LLM 401/403 | Immediate failure → deterministic_fallback | Same as above |
| LLM 5xx / unreachable | Immediately switch to deterministic_fallback | Same as above |
| Resolution missing any of 5 required fields | `validate_resolution` rejects | Written to `conflicts.jsonl`, resolution marked `rejected`, not entered into formal ledger |
| Resolution 5-state machine illegal transition | `advance()` returns `InvalidTransition` | Written to `conflicts.jsonl`, database is not overwritten |
| Export package write failure (disk full) | atomic_rename fails and cleans up temp directory | Exit code 7, no half-finished artifacts |
| Resolution item 6 required-field conflict / same-week resolution number duplication | Resolution number conflict written to `conflicts.jsonl` | Not entered into database |

### Integration with Existing Systems

- **OA (required integration)**: HTTP mode `GET /oa/events?week_id=…&cursor=…`, bearer auth; SQL mode queries against OA database read-only views. See `docs/integration.md` (with field mapping examples for 4 types: Landray / Seeyon / Weaver / in-house).
- **LLM (required integration)**: OpenAI-compatible `/v1/chat/completions`, single-protocol ModelGateway, business layer does not couple to vendor SDK.
- **DingTalk/WeCom (optional)**: Overdue and near-deadline notifications via webhook.
- **Not integrated**: HIS / EMR / medical record front page / medical insurance settlement interface / patient-identifiable details — this tool adheres to the business boundaries defined in CLAUDE.md.

## Typical Scenarios

### Scenario 1: Monday Batch (President's Office Secretary, Monday 09:30)

1. The secretary writes last week's executive meeting minutes md into `inbox/2026-W41/minutes.md` (habitual action)
2. Information systems engineer or on-duty assistant launches: `reso run --week 2026-W41`
3. Within 10 minutes they get 6 files + sha256 + audit in `out/2026-W41/`
4. The secretary checks `resolutions.csv` and `conflicts.jsonl`, confirms conflicts are reasonable, then signs off (or bounces back to the department to fill in fields)
5. The president's office sign-off sheet can be printed directly; the OA side is progressed by the secretary per established actions (this tool does not write back)

```bash
# Reproducible command sequence for this scenario
echo "## Resolution 1: …" > inbox/2026-W41/minutes.md   # Append section
reso run --week 2026-W41
ls -la out/2026-W41/                              # 7 files
cat out/2026-W41/summary.json                     # 6-bucket counts at a glance
```

### Scenario 2: Near-Deadline Follow-up (President's Office Director, Tuesday 10:00)

1. The director sees the `due_soon_count` in `summary.json` (e.g. 7 items) and decides to follow up today
2. Read `out/2026-W41/due_soon.jsonl`, aggregate by responsible department
3. Notify information systems / medical affairs / nursing department heads to proceed per established actions

```bash
# Reproducible command sequence for this scenario
jq -r '.[] | "\(.owner_department)\t\(.title)\t\(.due_at)"' out/2026-W41/due_soon.jsonl
# Paste the above into a DingTalk group to @ the corresponding department head
```

### Scenario 3: Audit & Traceback (Information Systems Engineer, Monthly Review)

1. End-of-month review: which resolutions experienced illegal transitions? How many times did the LLM extract, how many times did it degrade?
2. Read `audit_log.jsonl`, filter by `segment`: cli_start / oa_pull_complete / extract_call / bucket_advance / export_complete

```bash
# Reproducible command sequence for this scenario
jq -c 'select(.segment=="extract_call")' out/2026-W41/audit_log.jsonl
# Decision: whether to raise the fallback ratio / swap dictionaries / fix resolution item schema
```

## Output Examples

### Export Package `resolutions.csv`

> Real batch run snippet (mock stub environment, `/tmp/reso_screenshot_2026-W41/out/2026-W41/resolutions.csv`, end-to-end `run_handler` output).

```csv
# DEPLOYED_AT: 2026-10-12T12:26:19Z
# WEEK_ID: 2026-W41
# TOOL_VERSION: 0.1.0
# AI_DEGRADED: false
resolution_id,title,owner_department,owner_id,due_at,acceptance,status,due_in_days,due_bucket
#[W41]01,Pilot DRG admission validation,Medical Affairs,100086,2026-10-08,Orthopedics 3 DRG groups admission consistency rate ≥ 95%,completed,-4,overdue
#[W41]02,Outpatient EMR upgrade,Information Systems,200002,2026-10-13,5 outpatient departments all switched to EMR 5.0,completed,1,lt3
#[W41]03,Procure 5 ventilators,Equipment,300003,2026-11-10,5 ventilators delivered and accepted,completed,29,lt30
#[W41]04,Medical insurance settlement list format and process optimization,Medical Insurance Office,400004,2026-10-05,Medical insurance settlement list generated weekly,completed,-7,overdue
#[W41]05,Nursing documentation quality control special action,Nursing Department,500005,2026-12-31,Nursing documentation missing-item rate ≤ 5%,completed,80,gt30
#[W41]06,Annual energy consumption index control,Logistics,600006,2026-12-20,Unit-area energy consumption down 3% YoY,completed,69,gt30
```

> Column description: 6 required fields (resolution_id / title / owner_department / owner_id / due_at / acceptance) + 3 derived (status / due_in_days / due_bucket), fully consistent with the 6-bucket / 5-status fields in `summary.json`.

### Export Package `summary.json`

> Real batch run snippet (mock stub environment, 6 resolutions completed + 6-bucket classification + 6 departments).

```json
{
  "deployed_at": "2026-10-12T12:26:19Z",
  "week_id": "2026-W41",
  "tool_version": "0.1.0",
  "ai_degraded": false,
  "total": 6,
  "by_status": {
    "completed": 6
  },
  "by_department": {
    "Information Systems": 1,
    "Medical Insurance Office": 1,
    "Medical Affairs": 1,
    "Logistics": 1,
    "Nursing Department": 1,
    "Equipment": 1
  },
  "by_due_bucket": {
    "gt30": 2,
    "lt3": 1,
    "lt30": 1,
    "overdue": 2
  },
  "overdue_count": 2,
  "due_soon_count": 1
}
```

> Field description: `by_status` 5 buckets (pending / in_progress / due_soon / overdue / completed) + `by_due_bucket` 6 buckets (Lt3 / Lt7 / Lt14 / Lt30 / Gt30 / Overdue) + `overdue_count` and `due_soon_count` derived counts.

### Export Package `audit_log.jsonl` (5-Segment Example)

> Real batch run snippet (mock stub environment, 5-segment audit chain `cli_start` / `oa_pull_complete` / `extract_call` / `bucket_advance` / `export_complete`).

```jsonl
{"segment":"cli_start","tool_version":"0.1.0","commit_hash":null,"env_hash":null,"model_name":null,"prompt_hash":null,"latency_ms":null,"status":"ok"}
{"segment":"oa_pull_complete","tool_version":"0.1.0","commit_hash":null,"env_hash":null,"model_name":null,"prompt_hash":null,"latency_ms":null,"status":"ok"}
{"segment":"extract_call","tool_version":"0.1.0","commit_hash":null,"env_hash":null,"model_name":"test-model","prompt_hash":"7167aaf52be153fd8f94bda188a74d06c80fdd91961441f2116e23a8be524019","latency_ms":0,"status":"ok"}
{"segment":"bucket_advance","tool_version":"0.1.0","commit_hash":null,"env_hash":null,"model_name":null,"prompt_hash":null,"latency_ms":null,"status":"ok"}
{"segment":"export_complete","tool_version":"0.1.0","commit_hash":null,"env_hash":null,"model_name":null,"prompt_hash":null,"latency_ms":null,"status":"ok"}
```

> Field description: Each segment carries independent metadata `tool_version / commit_hash / env_hash / model_name / prompt_hash(SHA-256) / latency_ms / status`; in production `commit_hash` / `env_hash` are injected by CI.

### Export Package `overdue.jsonl` / `due_soon.jsonl` / `conflicts.jsonl`

> Real batch run snippet (mock stub environment).

```jsonl
# overdue.jsonl (2 overdue resolutions)
{"resolution_id":"#[W41]01","title":"Pilot DRG admission validation","owner_department":"Medical Affairs","owner_id":"100086","due_at":"2026-10-08","status":"completed","due_in_days":-4,"due_bucket":"overdue"}
{"resolution_id":"#[W41]04","title":"Medical insurance settlement list format and process optimization","owner_department":"Medical Insurance Office","owner_id":"400004","due_at":"2026-10-05","status":"completed","due_in_days":-7,"due_bucket":"overdue"}

# due_soon.jsonl (1 near-deadline resolution, ascending by distance to deadline)
{"resolution_id":"#[W41]02","title":"Outpatient EMR upgrade","owner_department":"Information Systems","owner_id":"200002","due_at":"2026-10-13","status":"completed","due_in_days":1,"due_bucket":"lt3"}

# conflicts.jsonl (empty file means no conflicts this batch; each conflict is one line)
{"kind":"InvalidTransition","resolution_id":"#[W41]07","from":"pending","to":"completed","reason":"must pass through started/in_progress event first"}
{"kind":"MissingField","resolution_id":"#[W41]08","missing":"acceptance"}
```

> Field description: `overdue.jsonl` streams every resolution with `due_in_days < 0`; `due_soon.jsonl` is sorted ascending by distance to deadline; `conflicts.jsonl` contains `kind` (InvalidTransition / MissingField / DuplicatedResolutionId / SchemaMismatch) + the rejected field or transition reason.

## Concept vs Actual Comparison

### Visual Goals

`docs/concepts/` contains the visual goal card (3-segment description: starting state / first usable moment / mature state, with color scheme, typography, information hierarchy, must-be-achievable constraints). This tool's interface is the CLI and export package files, so the "visual goal" mainly lands in the export package files themselves — the visual constraints are: 6 files + sha256 arranged neatly in `out/<week_id>/`, `summary.json` 6-bucket counts readable at a glance, `audit_log.jsonl` 5-segment audit chain structured for jq filtering. When there is no image-generation capability, only write a structured visual goal card; **do not fabricate images**.

### Actual Evidence

`docs/screenshots/` contains real batch run outputs (CLI sessions, export package snippets, audit_log snippets). All screenshots come from a real end-to-end `run_handler` batch run triggered by `cargo test --test screenshot_smoke` (mockito OA + mockito LLM + same-week fixture), then rendered to PNG by `tools/render_screenshots.py`. **Forbidden** to fake with design mockups or concept images.

![CLI Session](docs/screenshots/cli-session.png)

![Export Package Overview](docs/screenshots/export-package.png)

![5-Segment Audit Chain](docs/screenshots/audit-log.png)

![Main Resolution Table CSV](docs/screenshots/resolutions-csv.png)

> **Honest Note**: Left/top side are real batch run screenshots (mock stub environment, not production OA/LLM), not concept images or marketing graphics. This tool currently has no built-in image-generation capability, so `docs/concepts/` only contains the `visual-goal.md` structured visual goal card, **without fabricated images**. The terminal dark color scheme, font (STHeiti Medium), and information hierarchy (titles in orange-yellow, body text in light gray, key fields colored green/red/yellow) were rendered by Python+PIL according to the visual-goal.md color scheme; they are visually similar to a real macOS terminal but **there are necessarily differences** (line height, kerning, anti-aliasing details). Target vs actual output defers to the right-side batch run output.

## Architecture & Data Flow

```
┌──────────────────┐
│ inbox/<week_id>/ │  Meeting minutes md drop
│   minutes.md     │  (secretary action)
└────────┬─────────┘
         ▼
┌──────────────────┐    ┌──────────────────┐
│   extract module │◄───┤  ModelGateway    │  OpenAI-compatible
│ LLM extract +    │    │  (env config + 4 │  + fallback
│   validate +     │    │   error classes) │
│   fallback       │    │                  │
└────────┬─────────┘    └──────────────────┘
         ▼
┌──────────────────┐    ┌──────────────────┐
│   bucket module  │◄───┤  OA REST/SQL    │  bearer + paging
│ 5-state machine  │    │  (env config +   │  + retry
│ 6-bucket derive  │    │   retry + paging)│
└────────┬─────────┘    └──────────────────┘
         ▼
┌──────────────────┐
│   store module   │  rusqlite + DDL/migrate
│   (week, res)    │  + upsert_idempotent
│ primary key dedup│  + week idempotency key
└────────┬─────────┘
         ▼
┌──────────────────┐
│   export module  │
│ 6 files + sha256 │
│ + 5-segment audit│
└────────┬─────────┘
         ▼
┌──────────────────┐
│ out/<week_id>/   │
│ csv + jsonl +    │
│ summary + audit  │  ← secretary/department-head sign-off
└──────────────────┘
```

### Key Invariants

- **Idempotency key**: Same-week `week_id` + same-resolution `resolution_id` second `upsert_idempotent` does not overwrite, state unchanged.
- **Week hash**: `week_idempotency(week_id)` stores this week's resolution hash; a second batch run where the resolution items are unchanged can still idempotently export.
- **Strict state machine constraint**: Resolution status only advances within the sequence `pending → in_progress → due_soon → overdue → completed`, skipping returns `InvalidTransition`.
- **No silent data mutation on failure**: LLM unreachable → run `deterministic_fallback`; schema validation failure → write `conflicts.jsonl`; OA 4xx → immediate exit rather than silent swallowing.
- **Atomic export**: `out/<week_id>/` is written via `atomic_rename`, export failure leaves no half-finished directories.

## Security & Compliance Boundaries

- **This tool does not write back to OA**: After secretary/department-head sign-off, the OA side proceeds per established actions; the write-back loop is closed within the existing hospital OA workflow.
- **No patient-identifiable details**: Resolution items do not contain patient IDs, admission numbers, prescription numbers, or names; HIS/EMR/medical record front page/medical insurance settlement interfaces are not integrated.
- **Credentials always read from environment variables**: Bearer tokens, sqlite paths, and model keys never enter code or tests; `.env.example` is purely illustrative.
- **LLM failure must explicitly degrade**: Timeout, 401, 5xx, unreachable, schema mismatch — any one triggers `deterministic_fallback`; never silently treat "LLM didn't respond" as "resolution does not exist".

## Acceptance & Self-Check

| Check Item | Command | Expected Result |
|------------|---------|-----------------|
| Build passes | `cargo build --manifest-path resolution-watch/Cargo.toml` | Exit code 0, no warnings |
| Unit tests | `cargo test --manifest-path resolution-watch/Cargo.toml --lib` | All 7 module unit tests pass |
| End-to-end integration test | `cargo test --manifest-path resolution-watch/Cargo.toml --test integration_official` | mockito OA + LLM end-to-end, 6 files + sha256 + 5-segment audit chain all produced |
| 6-bucket boundaries | `cargo test --manifest-path resolution-watch/Cargo.toml --test bucket_integration` | 12+ boundary cases + cross-year ISO week pass |
| Store integration | `cargo test --manifest-path resolution-watch/Cargo.toml --test store_integration` | 8 unit tests pass |
| env missing immediate error | `OA_API_BEARER= cargo run --manifest-path resolution-watch/Cargo.toml -- run --week 2026-W41` | Exit code 1, stderr hints missing key |
| Export package sha256 | `sha256sum -c out/<week_id>/sha256.txt` | `resolutions.csv: OK` etc. all OK |
| Follow section | `test -f resolution-watch/qrcode.jpg && grep -q '## 关注我们' README.md && grep -q 'qrcode.jpg' README.md` | All three checks pass |

## Project Structure

```
resolution-watch/
├── README.md                      # This document (Chinese product manual)
├── .gitignore                      # Excludes .env / target / logs / score.json etc.
├── .env.example                    # Environment variable sample (10 items: OA + LLM + paths)
├── Cargo.toml                      # 13 core dependencies + dev-deps (mockito/tempfile/rstest)
├── qrcode.jpg                      # WeChat public account QR code (referenced by follow section)
├── config/
│   └── departments.yaml            # Hospital department dictionary (≥10 departments + aliases + keyword patterns)
├── docs/
│   ├── concepts/
│   │   └── visual-goal.md          # Structured visual goal card (3-state description + colors + information hierarchy)
│   ├── screenshots/                # Real batch run screenshots (CLI session / export package / audit chain / CSV table)
│   ├── integration.md              # 4-type OA field mapping + REST endpoints + SQL view examples
│   ├── architecture.md             # 6-bucket derivation + state machine details + ASCII data flow + mechanism comparison
│   └── examples.md                 # Export package 4 file snippets + 5-segment audit chain examples
├── schemas/
│   └── resolution.rs               # Resolution + ResolutionStatus + OaEvent + DueBucket + AuditSegment
├── src/
│   ├── lib.rs                      # Module declarations
│   ├── main.rs                     # tokio + tracing + dispatch
│   ├── cli/                        # clap subcommands + env validation + exit codes 0-7
│   ├── oa_pull/                    # reqwest outbound + bearer + 5xx retry + paging
│   ├── extract/                    # ModelGateway + prompt + validate + fallback + mock
│   ├── bucket/                     # 5-state machine + 6-bucket derivation + advance() rules
│   ├── store/                      # rusqlite DDL/migrate + upsert + aggregation + idempotency key
│   └── export/                     # csv + jsonl + summary + sha256 + atomic_rename
└── tests/
    ├── fixtures/                   # minutes md + OA events + LLM response
    ├── integration_official.rs     # End-to-end mock integration test
    ├── oa_pull_mock.rs             # 5 oa_pull unit tests
    ├── extract_mock.rs             # mock LLM + 2 unit tests
    ├── bucket_integration.rs       # 6-bucket boundary 12+ cases + cross-year
    └── store_integration.rs        # 8 store unit tests
```

## License

MIT

---

## Follow Us

Scan the QR code to follow our official account for updates and community access:

![Follow Us](qrcode.jpg)