# Task: <short title>

The contract is the JSON file, not this page. Copy `tasks/task.example.json`, fill it in
(`schemas/task.schema.json`), and store it during planning:

```sh
scripts/harness contract set path/to/task.json
```

`plan done` requires it, `build done` runs every acceptance `command`, the reviewer
receives it in the review packet, and `review done` refuses changes in `non_goals`
paths. A run with nothing to accept records why: `scripts/harness contract waive "<reason>"`.

Use this page only for notes that do not fit the contract:

## Context

Relevant docs, files, prior decisions. Harness root and target project root when they differ.

## Rollback

How to revert safely, including data or migration steps.
