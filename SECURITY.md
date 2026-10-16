# Security policy

## Reporting a vulnerability

Report vulnerabilities privately through [GitHub private vulnerability reporting](https://github.com/hraness/gobstopper/security/advisories/new). Please do not open a public issue for a suspected vulnerability.

Include the affected version (`gobstopper --version`), the command or request that triggers the problem, and what you expected. Do not attach real session transcripts; a minimal synthetic example is enough.

You can expect an acknowledgement within a few days. Fixes ship in a patch release, and the advisory credits the reporter unless they ask otherwise.

## Scope

Gobstopper runs locally. These areas are in scope:

- The `gobstopper proxy` loopback listener and the request bytes it forwards.
- The read-only `gobstopper mcp` server.
- The installers at `gobstopper.sh/install.sh` and `gobstopper.sh/install.ps1`, and release artifact verification.
- Snapshot vault handling and publication of separate session copies.
- The `gobstopper.sh` website.

## Supported versions

Security fixes land in the latest released version. Update with the installer or `cargo install --path crates/gobstopper-cli --locked` from a current checkout.
