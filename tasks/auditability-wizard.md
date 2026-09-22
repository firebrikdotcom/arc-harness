# Task: Auditability planning wizard

## Goal

Provide a standalone local wizard that turns the unanswered design questions for the harness audit trail, TypeSafe/JEV evaluation, and Arc-backed improvement loop into a reviewable Markdown plan.

## Context

- The current harness records route and advice calls, but does not record every session or uncalled JEV opportunity.
- The proposed durable architecture is event-sourced: commands establish intent, facts are appended, and projections provide review views.
- Raw prompts may contain sensitive project material; global analysis must work from fingerprints and redacted classifications by default.

Harness root and target project root: this repository checkout.

## Acceptance Criteria

- [x] A local browser wizard covers scope, event design, prompt privacy, capture coverage, JEV opportunities, outcomes, projections, distributed delivery, security, operations, and rollout.
- [x] It produces a portable Markdown plan and JSON answers without network requests or a build step.
- [x] It clearly identifies unanswered required decisions and is keyboard accessible.
- [x] Regression coverage confirms the essential question groups and export behavior are present.

## Constraints

- Do not collect or export raw prompts by default.
- The wizard is planning support only; it does not route work or call JEV.
- Keep it dependency-free and runnable from a local file.

## Verification Plan

- Run the focused wizard regression test.
- Run `scripts/verify.sh` and `scripts/review.sh`.
