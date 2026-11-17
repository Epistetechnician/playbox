# Security policy

Playbox is security software: a flaw in grants, sandboxing or the signed history matters more than most bugs. Please report it privately.

## Reporting a vulnerability

Use GitHub's private reporting: **Security → Report a vulnerability** on this repository
(<https://github.com/Epistetechnician/playbox/security/advisories/new>). Do not open a public issue or pull request for a vulnerability.

Include what you found, how to reproduce it, the version (`playbox --version`) and what an attacker gains. We aim to acknowledge within
3 working days and to give a first assessment within 10. We will agree a disclosure date with you and credit you in the advisory unless you
prefer otherwise.

## Supported versions

The newest release gets fixes. Older releases are fixed only for critical issues, and only when the fix does not change the wire protocol.

## Scope and limits

What Playbox promises, and what it does not, is written down: [docs/threat-model.md](docs/threat-model.md), [docs/requirements.md](docs/requirements.md)
and the "What is not proven" section of the [README](README.md). A report that breaks a stated promise is in scope. Run `playbox secure` to check your own install.

## Verifying a release

Every release lists its files in a signed `SHA256SUMS` and carries a build-provenance attestation; see [docs/releasing.md](docs/releasing.md).
