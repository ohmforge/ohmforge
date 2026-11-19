---
name: Bug report
about: Report something that is broken or behaves incorrectly
title: "bug: "
labels: ["bug"]
---

<!--
Thanks for helping improve ohmforge! Please search existing issues first.
For questions or help (not a bug), use Discussions instead:
https://github.com/ohmforge/ohmforge/discussions
Do NOT report security vulnerabilities here — see SECURITY.md.
-->

## Summary

A clear, concise description of the bug.

## Reproduction

Steps to reproduce. A minimal `.eesh` script or netlist is ideal:

```eesh
# paste a minimal script that triggers the bug
```

Or the exact command(s) you ran:

```sh
# e.g. cargo run -p ohmforge --bin of -- ...
```

## Expected behavior

What you expected to happen.

## Actual behavior

What actually happened. Include the full output, error message, or stack trace (in a code block).

## Environment

- ohmforge version / commit: <!-- e.g. 0.8.1 or a git SHA -->
- OS and version: <!-- e.g. macOS 15.0, Ubuntu 24.04, Windows 11 -->
- Rust toolchain (`rustc --version`): <!-- if building from source -->
- Relevant features enabled: <!-- e.g. http, relay, toolchain -->

## Additional context

Screenshots, logs, or anything else that helps.
