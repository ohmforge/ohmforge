# Governance

This document describes how **ohmforge** is run. It is deliberately lightweight and reflects the project as it actually operates today; it will grow as the community does.

## Project model

ohmforge is a **maintainer-led, open-core** project. This repository is the **free, open-source core**, licensed under [Apache-2.0](LICENSE-APACHE). Some advanced or hosted capabilities may be offered separately as commercial add-ons; the core in this repository is and will remain open source.

There is no foundation or formal legal entity behind the project today — it is stewarded by its maintainers. We would rather be honest about that than invent governance we do not practice.

## Roles

- **Users** — anyone who uses ohmforge. Feedback, questions ([`SUPPORT.md`](SUPPORT.md)), and bug reports are contributions too.
- **Contributors** — anyone who has a change (code, docs, tests, examples) accepted. See [`CONTRIBUTING.md`](CONTRIBUTING.md).
- **Maintainers** — contributors trusted with review and merge rights, responsible for the roadmap, releases, and upholding the [`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md).

## How decisions are made

Most decisions happen in the open on pull requests and in [GitHub Discussions](https://github.com/ohmforge/ohmforge/discussions), by **lazy consensus**: a proposal with no sustained objection, after reasonable time for review, is accepted.

- **Routine changes** (bug fixes, docs, tests, additive features that respect the existing contracts) need review and approval from one maintainer.
- **Changes to cross-track contracts** (the `Circuit` API, `Signal`, `Analysis`, `McuCore`, the `Command` AST, the project schema) follow the checkpoint process in [`docs/plan/`](docs/plan/README.md) and should have maintainer agreement before merging.
- **Disagreements** are resolved by discussion first. If consensus cannot be reached, the maintainers decide; the goal is the long-term health of the project, not winning an argument.

Every change keeps the project green: `just check` (fmt + `clippy -D warnings` + tests + docs) must pass, and user-visible features ship with an example and a `help` entry.

## Becoming a maintainer

Maintainers are drawn from active contributors. There is no quota and no fixed timeline. What we look for:

- a track record of quality contributions that respect the project's contracts and testing bar,
- good judgment in review and constructive, kind collaboration,
- reliability — following through on what you pick up.

Existing maintainers invite new ones by consensus. If you are interested, the path is simply to keep contributing; it is also fine to express interest in a Discussion.

## Contribution terms: DCO today, CLA maybe later

Contributions are accepted under the Developer Certificate of Origin (**DCO**) — a per-commit `Signed-off-by` sign-off (`git commit -s`), as described in [`CONTRIBUTING.md`](CONTRIBUTING.md). A **Contributor License Agreement (CLA)** may be introduced before accepting large external contributions as governance matures; if that happens it will be announced in `CONTRIBUTING.md` with clear instructions. For now, a DCO sign-off is all that is required.

## Releases

Releases follow [Semantic Versioning](https://semver.org/) and are recorded in [`CHANGELOG.md`](CHANGELOG.md). During the pre-1.0 phase, security fixes land on the latest `0.x` release (see [`SECURITY.md`](SECURITY.md)).

## Trademarks

The ohmforge **code** is open source; the ohmforge **name and logo are trademarks** of the maintainers. See [`TRADEMARK.md`](TRADEMARK.md) for what you may do without asking and what needs permission.

## Code of conduct

Everyone participating in ohmforge spaces is expected to follow our [`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md).

---

*This document describes project practice, not a binding contract, and is not legal advice. Where it appears to conflict with the [Apache-2.0 license](LICENSE-APACHE) as applied to the code, the license governs the code.*
