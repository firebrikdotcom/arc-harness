# Harness helper targets. Verification detection in scripts/verify.sh only
# reacts to format/lint/typecheck/test/build targets, so keep those out of here.

PROJECT ?= .

.PHONY: help install-guides

help:
	@printf '%s\n' \
	  'make install-guides [PROJECT=/path/to/project]' \
	  '    Add or refresh the harness command block in AGENTS.md and CLAUDE.md.'

install-guides:
	@scripts/install-guides.sh --project "$(PROJECT)"
