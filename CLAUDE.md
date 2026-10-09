# Claude Harness Instructions

The operating guide is AGENTS.md, imported here so both agents read one source:

@AGENTS.md

Claude Code specifics:

- The phase guard runs as a PreToolUse hook from `.claude/settings.json`. The repeated-failure stop, the todo gate, and the session brief run from the hooks `scripts/install-hooks.sh` installs.
- For the independent review, hand `review-packet.md` (path printed by `scripts/review.sh`) to a subagent that has not seen this conversation, or set `HARNESS_REVIEWER_CMD`.
