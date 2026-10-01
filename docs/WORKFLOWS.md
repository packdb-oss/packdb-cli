# Required OSS workflows

The adjacent YAML files are staged workflow templates. Copy them into the destination
`.github/workflows/` only with the repository adapters below. They intentionally fail
when an adapter is absent; no placeholder can count as a passing release gate.
These templates have no effect from this documentation directory.

## Required checks

| Workflow/job | Trigger | Required evidence |
| --- | --- | --- |
| `oss-ci / checks` | Every PR, main push, merge group; weekly/manual full checks | Policy/contact completeness, format, Clippy, unit/doc tests, MSRV and affected shipping features; stable/broader matrix at scheduled/release gates, package contents, license/provenance checks, source license notices (docs/SOURCE_HEADERS.md) |
| `oss-ci / coverage` | Same | Per-file 80% gate with missing-data failure, declared features, canonical evaluator self-test and exception verification |
| `oss-ci / compatibility` | Same | Parser corpus; SDK protocol/feature tests; CLI process/HTTP/Flight SQL tests; server three-signal ingest/query/restart and auth/config |
| `oss-security / audit` | PR, main push, weekly | Fresh RustSec data, approved dependency licenses, secret scan of change/full export before release; exceptions have owner, scope and expiry |
| `oss-candidate / candidate` | Manual dispatch on main | Exact SHA, release checks, package/source inventories and target binaries with checksums |
| `oss-publish / publish` | Manual dispatch on main, disabled by default | Protected environment, approved commit/tag, existing candidate verification, registry and GitHub publication in dependency order |

Require all stable job names in branch rules; do not use path filters that omit
required checks. Protect main and tags, dismiss stale approvals, require code-owner
review for workflows/auth/storage, and restrict bypass. Dependabot updates Cargo
and GitHub Actions weekly; its PRs pass the same checks. `oss-action-pin-watch`
(weekly, read-only, `scripts/check_action_pins.py`) fails when a pinned tag has
moved or a pin trails the action's latest stable release. Enable secret scanning/push
protection where available, with an equivalent local/CI scan where unavailable.

Run untrusted PR code on disposable GitHub-hosted runners with contents:read,
no secrets and persist-credentials:false. No `pull_request_target`, privileged
workflow_run, self-hosted runner, or AI bot with write tokens for PR execution.
External Actions are pinned to full commit SHAs resolved from the official v4 tags
on 2026-09-17. Review those revisions before activation and preserve immutable pins
on updates. Set timeouts, bounded
artifact retention, and inspect workflow changes like production code.

## Repository adapters required before activation

Provide an executable `scripts/oss-ci.sh` with a strict subcommand interface below.
Reject unknown commands and propagate failures (`set -euo pipefail`); do not use
`continue-on-error` or success fallbacks. Reuse existing Makefile wrappers, package
selection and coverage evaluator rather than implementing competing gates.
`OSS_CHECK_SCOPE=affected` selects the reviewed PR change classifier; `full` selects
all declared shipping combinations and MSRV/stable checks. Scheduled/manual CI and
candidates set `full`. Full coverage checks score all selected executable source
when there is no change base; a missing PR change base must still fail.

### Adapter inputs

The positional subcommand is authoritative. These are environment inputs, not shell
fragments; validate them before use, quote them, and never evaluate them as commands.
Required inputs missing or malformed for the selected mode fail closed.

| Input | Supplied by / required use |
| --- | --- |
| `OSS_GATE` | CI matrix only; equals positional `checks`, `coverage` or `compatibility`. Other workflows invoke their subcommand directly and do not set it. If present, require agreement with the argument. |
| `OSS_CHECK_SCOPE` | CI/security set `affected` for PR/push/merge-group, `full` for schedule/manual runs; candidate sets `full` for its validation gates. Publish uses its separate immutable candidate contract and does not set scope. |
| `OSS_BASE_SHA` | CI/security: PR base SHA, merge-group base SHA, or push `before`. Required and resolvable in affected mode; empty is permitted only in full mode. Validate as a commit, never substitute the default branch silently. |
| `OSS_HEAD_SHA` | CI/security: PR head SHA or event SHA. Required resolvable commit; defines the changed-source range endpoint. PR checkout normally tests GitHub's synthetic merge commit, so record both the range head and actual tested checkout SHA. |
| `OSS_SOURCE_SHA` | Candidate/publish: exact workflow commit; require equality with checked-out HEAD and the candidate release record. |
| `OSS_CANDIDATE_RUN_ID` | Publish only: required positive integer from the reviewed dispatch input; locate the candidate in the same repository and expected candidate workflow, verifying its commit and successful status. |
| `OSS_CANDIDATE_RUN_ATTEMPT` | Publish only: required positive integer selecting the reviewed attempt within that run; verify that attempt's success and matching artifact identity. |
| `GITHUB_RUN_ID`, `GITHUB_RUN_ATTEMPT` | GitHub-provided candidate execution identity; candidate records both, with the expected artifact name, in `release-record.json`. A rerun retains the run ID but changes attempt. |
| `GH_TOKEN` | Publish only: scoped GitHub API credential for candidate verification and release publication; never log or embed it in an artifact. Registry authentication remains separate. |

`OSS_PRIVATE_SCHEDULES_ENABLED` and `OSS_PUBLIC_RELEASE_ENABLED` are repository
configuration variables evaluated by GitHub job conditions, **not** adapter
environment inputs. Their defaults and activation rules are specified below.

For affected scope, PR and merge-group changes use the merge-base of validated
base/head commits; pushes compare the validated before/after trees directly (including
non-ancestor bases after force-pushes). Empty, all-zero, malformed or unresolvable
bases are unusable. Fetch a valid missing commit when possible; otherwise fail the
affected gate. A first push with a zero base needs an explicit full-scope run rather
than a fabricated change range. Full runs scan/score the entire selected source set.

### Adapter subcommands

- `checks`: validate policy fields/contacts, action pins, clean-export rules, source license
  notices (`scripts/check_source_headers.py`), license and package allowlists;
  `cargo fmt --all -- --check`; Clippy warnings/dead-code errors;
  unit/integration/doc tests and `cargo doc` with warnings denied. Test MSRV 1.94.1
  on affected PRs and stable independently weekly/at release or when toolchain
  configuration changes. For each selected crate, test no-default/default and
  affected shipping features; run the broader declared combinations weekly/at release; compile shipping features in coverage.
  Package verification must include standalone resolution, not only monorepo builds.
- `coverage`: create LCOV for the actual repository source layout, determine changed
  files using the validated input/range rules above, and invoke the copied canonical
  `scripts/coverage_gate.py`. Treat zero or unresolvable bases as missing evidence:
  fetch a valid commit or fail, never silently score another range. Full scheduled/
  release mode scores the full selected source set. When no executable source changed,
  the evaluator may report an explicit skip; missing coverage for a changed executable
  source file must fail. Carry the evaluator self-test and owner-list checks.
- `compatibility`: select the repository-specific suites in the table above. Never
  make parser testing wait for a deployed PackDB server. Bring corpus manifests,
  baseline evaluators and required test services into the destination repository.
- `security`: install reviewed pinned scanner versions, fetch fresh advisories,
  audit Cargo.lock and all release features, check dependency licenses and secrets.
  Advisory/license checks cover the full selected dependency graph in either scope.
  In affected scope, secret scanning covers the validated change range; full scope
  covers the complete export and release-history surface. Use the same invalid-base
  handling as coverage. Candidate validation requires a full security scan.
  Network failure is unavailable evidence, not a clean security verdict.
  The SDK's reviewed Arrow 60 and OpenTelemetry 0.33 upgrades removed the
  earlier `paste` and baggage-parser advisory exceptions. The HTTP and gRPC
  baggage input bounds remain as a defensive limit; no advisory is ignored.
- `candidate`: require successful preceding gates for the exact source SHA. Reuse
  trusted same-SHA results and artifact hashes; rerun only missing, changed or stale
  gates (refresh advisory checks at release). Build the declared target matrix, smoke-test
  every archive, and write `dist/` containing assets, checksums, SBOM/source inventory
  and `release-record.json` with source SHA, version, target, feature/toolchain and
  gate evidence, candidate run ID/attempt and artifact name. Use
  `candidate-SHA-RUN_ID-RUN_ATTEMPT` so attempts have distinct immutable names.
  Upload only `dist/`, never credentials or raw support bundles.
- `publish`: verify the current SHA equals the approved candidate SHA; verify a
  protected tag points there, all required checks succeeded and the record/artifact
  hashes match. Obtain `candidate-SHA-RUN_ID-RUN_ATTEMPT` from the explicitly
  reviewed run and attempt, never “latest”. Verify the record matches all three
  identifiers and the artifact hash; reject missing, ambiguous or expired artifacts.
  Verify explicit per-package versions/allowlist. Reject private repository visibility
  for this public launch job. Publish dependency-first, verify registry state and
  clean installs, then publish the reviewed GitHub Release. Never build a different
  artifact in this privileged step or execute code downloaded from untrusted PRs.

These adapters are extraction deliverables because package names, feature selections,
source layout and platform support differ across destinations. The workflow templates
are orchestration contracts, not a claim those uncreated repositories passed CI.

## Environment and release configuration

Create `private-candidate` and `public-release` environments. Public-release needs
an independent reviewer, main-only deployment rules and scoped credentials/OIDC
trust. Verify enforcement on the organization's private-repo plan. Keep repository
variable `OSS_PUBLIC_RELEASE_ENABLED` absent or `false` until launch. Its presence
is not a substitute for the environment review and SHA-bound release record.

Keep repository variable `OSS_PRIVATE_SCHEDULES_ENABLED` absent or `false` during
private staging. Setting it to the literal `true` enables **both** scheduled CI and
scheduled security audits for private repos, after the shared budget is measured.
Public repos run both schedules regardless of this variable. PR, push, merge-group
and manual runs are unaffected; required PR/release security checks stay mandatory.
These are GitHub repository variables (`vars`), not shell environment variables.

The candidate workflow grants no registry credentials. Add a separate narrowly scoped
job if uploading to a private draft GitHub Release; ordinary Actions artifacts suffice
for early rehearsals. The publish workflow grants contents:write and id-token:write
only to its gated job. Bootstrap tokens, if needed for first publication, must be
added only to that environment and removed immediately after bootstrap.

For binaries, expand candidate builds to native target jobs on the tested platforms
in RELEASING.md, then aggregate immutable per-target artifacts. A Linux-only workflow
run cannot verify Windows or macOS. Libraries need no binary archive matrix. Run
scheduled parser fuzzing/compatibility drift and server recovery/performance suites
where applicable, recording failures for triage; these complement required PR gates.

Before activation: replace every contact/command placeholder, add the real adapters,
review action SHA pins, configure rules/environments, explicitly record
`OSS_PRIVATE_SCHEDULES_ENABLED` and `OSS_PUBLIC_RELEASE_ENABLED` settings, run
actionlint, and demonstrate a
failing adapter blocks merge/publication plus a successful private candidate. Public
publication is a later action, not an automatic consequence of installing this kit.

## Strict checks within the OSS Actions allowance

Checked 2026-09-17: standard GitHub-hosted runner execution is free for public
repositories; larger runners remain billable. Private staging consumes the owner's
plan allowance. GitHub Free for organizations currently includes 2,000 minutes/month,
500 MB of shared artifact/Packages storage and 10 GB cache per repository. These are
account/plan limits, not a separate free grant to each private repo. Confirm the real
organization plan and usage before activation. [GitHub Actions billing](https://docs.github.com/en/billing/concepts/product-billing/github-actions).

Public execution is still subject to runner capacity and usage policies; do not
assume free standard-runner time means unlimited compute or storage. No large paid
runners, persistent self-hosted runners, GPU jobs, commercial workloads or paid
Actions are required by the OSS baseline. Keep commercial soak and deployment
work in the internal org. [GitHub Actions usage](https://docs.github.com/en/actions/concepts/billing-and-usage).

Use this operating budget, not an unmeasured promise that the builds already fit:

- Start private staging on standard Ubuntu only, with at most two gate jobs per
  workflow and one active revision per PR. Cancel superseded CI/security runs.
  Provision repos sequentially if the shared quota cannot support concurrent work.
- Measure cold/warm wall times on the first parser, SDK and server PR. Budget **all
  five repos together** against remaining included minutes; reserve 20% for fixes.
  For example, 40 aggregate PR revisions at 30 billed Linux minutes plus four
  100-minute candidates totals 1,600 minutes. This is a planning envelope, not an
  observed runtime or additional allowance. Include audit/scheduled/OS jobs in the
  total; lower run counts if actual costs exceed the envelope.
- Required PR checks stay on every PR with stable names. Use a reviewed diff classifier
  inside gates: documentation-only changes run policy/link/workflow checks and
  report explicit source-test non-applicability; Rust/manifests/build scripts,
  generated inputs and workflow changes trigger affected compiled checks. Unknown
  changes default to full checks. Coverage of changed executable code never skips.
- Run MSRV and selected shipping features on affected code; use stable/latest and
  the broader feature/OS matrix weekly and at release. A toolchain or feature change
  also runs its affected matrix on the PR. Every advertised binary target is built
  and smoke-tested for a release. Do not run the full OS cross-product on every PR.
- Narrow parser/SDK/CLI gates to their own dependency closures. Server coverage
  keeps the 80% per-file rule; use the existing evaluator and relevant suites rather
  than compiling code outside the destination repository. A failure or missing data still blocks.
- Cache only reusable Cargo inputs/build outputs with keys including OS, toolchain,
  features and lockfile. Keep within the included cache limit; no paid cache expansion.
  Privileged builds must not trust caches/artifacts written by untrusted PRs.
- Do not upload build trees, corpora or raw logs by default. Candidate artifacts
  expire in three days; the template caps each upload at 100 MiB before upload.
  The adapter checks aggregate org storage and reserves headroom before building:
  use a 350 MiB working ceiling on a 500 MB allowance, including existing Packages
  usage. Remove obsolete candidates or wait; never silently buy more storage.
  If full server assets exceed this cap, use a reviewed draft GitHub Release for
  intended binary distribution and retain only its hash manifest in Actions.
  Failed/expired candidate evidence must be rebuilt before publication.
- Disable scheduled heavy jobs during private staging by default; enable weekly
  drift/fuzz jobs after measuring the budget. Security audit on PR/release remains
  required. Use short bounded fuzz smoke tests on parser PRs, longer scheduled
  fuzzing within budget, and private funded infrastructure for extended campaigns.
- Check org usage weekly and before candidate builds; configure alerts and a
  stop-spending budget where supported. At 80% of the private allowance, pause
  discretionary runs and sequence remaining work. At exhaustion, wait for reset
  or request a deliberate plan/budget decision. Never turn required checks green
  because budget ran out. A plan names intended limits; only measured runs plus
  account usage can confirm the actual fit.

Enforceable protections vary by GitHub plan. During private staging, if required
reviews/environments are unavailable, keep publishing disabled and have a maintainer
run the reviewed candidate process without release credentials in PR CI. Public
activation requires verified branch/tag and publication controls; do not assume a
paid-plan feature or a merge queue is required. `merge_group` is ready if a supported
queue is enabled, but ordinary protected PR merges remain sufficient.

## This repository's one departure: the sibling SDK checkout

`packdb-query-cli` depends on `packdb-client` **by version**. Until that crate
is published there is no registry to resolve it from, so `Cargo.toml` patches it
to a sibling `packdb-sdk` checkout. Every gate above resolves dependencies —
clippy, the test matrix, `cargo llvm-cov`, and `cargo deny`'s metadata read — so
**every gate needs that sibling**. `cargo metadata --no-deps` and
`cargo package --list` are the exceptions; they never resolve, so they succeed
without it and must not be mistaken for evidence that the workspace is sound.

Consequences a reader should not have to rediscover:

- Both repositories are checked out into subdirectories (`packdb-cli/`,
  `packdb-sdk/`) so the layout the manifest expects actually exists. Jobs set
  `working-directory: packdb-cli`.
- `GITHUB_TOKEN` is scoped to this repository, so a private `packdb-sdk` needs
  its own credential. Each job mints a **short-lived GitHub App installation
  token** with `actions/create-github-app-token`, scoped to `packdb-oss` /
  `packdb-sdk` alone with `permission-contents: read`. It expires with the job,
  can write nowhere, and never reaches a shell: the action returns it as a step
  output and `actions/checkout` consumes it directly with
  `persist-credentials: false`. The workflows read
  `secrets.OSS_SDK_APP_CLIENT_ID` and `secrets.OSS_SDK_APP_PRIVATE_KEY` as action
  inputs, never interpolating either into `run:` script text. Configure both
  names through the approved secrets runbook in **Actions secrets and
  Dependabot secrets** for this repository. GitHub supplies only Dependabot
  secrets to workflows triggered by Dependabot PRs; an Actions-only setup
  passes human PRs but leaves every Dependabot update unable to mint a token.
  The App installation must be limited to `packdb-oss/packdb-sdk` with
  Contents: read. Verify one human PR and one Dependabot PR before requiring
  the four check contexts on `main`.
- The SDK checkout is pinned to a **reviewed full commit SHA**, `SDK_COMMIT` at
  workflow level, and a step fails the job if what arrived is not that commit.
  A moving branch would let a later SDK change alter a CLI candidate with no
  CLI review, and the binary's dependency closure would differ from the one
  that was signed off. Bump it deliberately in a pull request and review
  the lockfile. Regenerate it if the SDK dependency graph changed; a
  metadata-only SDK update can leave the reviewed lockfile byte-identical.
- Dependabot cannot see `packdb-client` or `packdb-otel` and is configured to
  ignore them. They move when `SDK_COMMIT` is bumped to a new `packdb-sdk`
  release in a reviewed pull request.

All of this is deletable in one change once the SDK is published: remove
`[patch.crates-io]`, the App-token and sibling-checkout steps, the pinned
`SDK_COMMIT` with its confirmation step, and the `require_sdk_sibling` check.
