# Contributing

Welcome contributions to code, documentation, examples, tests and accessibility.
Read CODE_OF_CONDUCT.md, CODING_STANDARDS.md and AI_POLICY.md before contributing.
Use SUPPORT.md for questions and SECURITY.md for private vulnerability reports.

Start small: reproduce the problem, search existing issues, and describe expected
behavior. Discuss major API or architecture changes in an issue before writing a
large patch; small fixes need no advance permission. Maintainers label approachable
issues and identify a contact for questions. Commercial affiliation does not change
review standards.

Install the toolchain in rust-toolchain.toml and run `scripts/oss-ci.sh checks`.
New Rust, Python and shell files open with the Apache 2.0 notice described in
docs/SOURCE_HEADERS.md; `checks` rejects a file without it.
Repository-specific integration services and platform requirements belong in README.
Keep tests and docs with the behavior they cover. Use synthetic reproducers and never
post credentials or customer data. Include commands/results, compatibility impact
and known limits in the pull request. Reviewers may ask for smaller changes.

Contributions are accepted under Apache-2.0, the repository's outbound license.
Submit only work you have the right to contribute, preserving third-party notices
and disclosing copied/adapted material with its origin and license. No copyright
assignment is requested. This template does not introduce a separate CLA or DCO
sign-off requirement. Any later change to contribution terms needs a published
policy decision and cannot be silently applied retroactively.

At least one independent maintainer approves a normal PR; security-sensitive,
format-breaking and release changes receive a second maintainer review. All required
checks must pass on the reviewed revision. No self-merge of unreviewed changes.
While MAINTAINERS.md lists a single maintainer, independent approval is not
possible; see that file for the interim rule and when it ends.
Maintainership decisions follow GOVERNANCE.md. Contributors retain authorship and
are welcome to help review and maintain their changes.
