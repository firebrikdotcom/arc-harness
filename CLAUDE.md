# Claude Harness Instructions

The operating guide is AGENTS.md, imported here so both agents read one source:

@AGENTS.md

Claude Code specifics:

- The phase guard runs as a PreToolUse hook from `.claude/settings.json`. The repeated-failure stop, the todo gate, and the session brief run from the hooks `scripts/install-hooks.sh` installs.
- For the independent review, the agent cannot approve its own work: `harness review submit` asks for `yes` typed on a terminal. Either a person writes a reviewer command into `.harness-db/reviewer` so `scripts/review.sh` runs a fresh-context reviewer itself, or the agent hands `review-packet.md` (path printed by `scripts/review.sh`) to a subagent that has not seen this conversation and asks the user to run `scripts/harness review submit FILE` in their own terminal.
