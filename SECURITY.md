# Security Policy

We take the security of ohmforge seriously and appreciate responsible disclosure.

## Reporting a vulnerability

**Please do not report security vulnerabilities through public GitHub issues, pull requests, or discussions.**

Instead, report them privately through one of:

- GitHub's **[private vulnerability reporting](https://github.com/ohmforge/ohmforge/security/advisories/new)** ("Report a vulnerability" under the repository's *Security* tab) — preferred.
- Email the maintainers at **security@ohmforge.dev**.

Please include, as best you can:

- a description of the issue and its impact,
- the affected version, crate, or component,
- step-by-step reproduction (a minimal `.eesh` script or netlist is ideal),
- any proof-of-concept, logs, or stack traces.

## What to expect

- We aim to **acknowledge** your report within **3 business days**.
- We will keep you updated as we investigate and work on a fix.
- Once a fix is available we will coordinate a disclosure timeline with you and credit you (if you wish) in the release notes and advisory.

## Scope

ohmforge is a simulation and emulation workbench that runs locally and, only with the opt-in `http` feature, talks to the Claude API. Security-relevant areas include: parsing of untrusted project/netlist files, the firmware emulators, and the optional network transport and signaling relay. Reports about any of these are welcome.

## Supported versions

Security fixes land on the latest release. During the pre-1.0 phase only the most recent `0.x` release is supported.
