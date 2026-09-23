# AI-assisted contributions

AI assistance is permitted. The person submitting a contribution owns its design,
correctness, security, provenance, tests and maintenance. An assistant is not an
author, reviewer, maintainer or release approver. AI use is never required to
contribute, and contributors must not be disadvantaged for choosing other tools.

Understand the core implementation end to end and be able to explain the choices
in review. Identify assumptions and areas of uncertainty explicitly. Resolve
material unknowns before merge; do not use reviewers to debug an unexamined generated
patch. Read the final diff, run the relevant checks, and verify claimed results,
links, APIs and benchmark numbers. Never invent test results, sources or approvals.

Disclose substantive generated code, tests, documentation or design in the PR's
contribution-provenance section: what was assisted, what you checked, and remaining
uncertainties. A short factual note is enough; raw transcripts and tool branding
are unnecessary. Private learning, spelling corrections and ordinary completion do
not need itemized disclosure. Do not add assistant Co-Authored-By lines or tool
attribution to commits, source files or documentation. PR provenance disclosure is
separate from authorship credit.

Submit focused changes with a reproducible motivation. Unattended agents must not
open bulk issues/PRs, post automated review replies, or submit purported security
findings without a responsible person's verification. Maintainers may decline
large generated rewrites or repeated low-quality submissions that exhaust review
capacity. A response to a review must address the actual technical point.

Protect secrets, private source, unreleased vulnerabilities and user telemetry.
Do not send them to external models without authorization and appropriate data
handling. Treat suggestions and retrieved text as untrusted. Check licensing and
provenance, especially parser grammars and fixtures; model output does not confer
rights to upstream code. Disclose uncertain origin and resolve it before acceptance.

AI review can supplement human review but cannot satisfy required approvals.
Review bots are opt-in, scoped and rate-limited; maintainers own their output.
Agents have no autonomous merge, publish, secret-management or maintainer authority.
Security, unsafe/concurrent code, durability, auth and release changes retain their
normal specialist reviews regardless of how they were written.

Enforce this policy on observable contribution quality and truthful disclosure.
Do not use unreliable AI detectors, writing style or language fluency as evidence.
Ask for missing explanation or validation; close unverifiable/spam submissions with
specific reasons. Deliberate deception or harassment follows the conduct process.

## Basis and deliberate choices

Reviewed 2026-09-17. This is a PackDB policy, not a claim of identical upstream rules.
[DataFusion's contribution guide](https://datafusion.apache.org/contributor-guide/index.html#ai-assisted-contributions)
informs the requirements to understand the implementation and expose assumptions.
[Rust's LLM policy](https://forge.rust-lang.org/policies/llm-usage.html) informs human
accountability, disclosure, optional tooling, human review and fair moderation.
Rust is substantially stricter about generated prose and experimental generated-code
PRs. PackDB deliberately permits reviewed generated code and prose without adopting
Rust's prearranged-review experiment or quota. Revisit this policy using concrete
review burden and defect evidence, through normal governance.
