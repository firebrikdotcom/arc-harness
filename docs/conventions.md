# Conventions

## Core Rule

Follow existing patterns first. If the repository already has a style, test pattern, naming convention, framework idiom, or error-handling approach, use it before introducing a new one.

## Coding Style

- Prefer simple, explicit code over clever abstractions.
- Keep functions and modules focused on one responsibility.
- Use formatting tools configured by the project.
- Avoid broad refactors during feature or bug-fix tasks unless required for correctness.
- Leave succinct comments only where they clarify non-obvious decisions.

## Naming

- Use descriptive names that match the language and framework conventions.
- Name modules by responsibility, not implementation detail.
- Name tests after the behavior they verify.
- Avoid abbreviations unless they are already common in the codebase.

## Testing

- Add or update tests when behavior changes.
- Prefer fast, deterministic tests.
- Cover meaningful behavior and edge cases, not implementation trivia.
- Keep fixtures small and local unless shared fixtures already exist.
- If a test cannot be added, say why in the review findings or the handoff.
- For starter projects, prefer native checks first: `go test`, `composer` scripts, `cargo test`, package-manager scripts, and shell syntax/lint checks.

## Error Handling

- Fail loudly for invalid states, missing required configuration, and unsafe operations.
- Return actionable error messages.
- Avoid swallowing exceptions silently.
- Preserve original error context where the language supports it.

## Logging

- Log useful operational events, not noisy implementation details.
- Do not log secrets, credentials, tokens, private keys, or personal data.
- Use structured logging if the project already uses it.
- Keep logs actionable for debugging and support.

## Security

- Never commit secrets.
- Do not assume secrets are available in CI.
- Validate untrusted input at boundaries.
- Use dependency versions and tooling already established by the project.
- Prefer safe defaults for scripts: no destructive operations, no network-only success paths, and no implicit production access.

## Documentation

- Update `docs/setup.md` when setup, commands, dependencies, or environment variables change.
- Update `docs/architecture.md` when module boundaries or dependencies change.
- Record meaningful steps with `scripts/harness step --note`.
- For cross-project orchestration, keep target-specific registries, task notes, run logs, generated indexes, and project documents in ignored local database state such as `.harness-db/`.
- Do not track project database records in this template repo unless they are intentionally generalized into reusable template documentation.
