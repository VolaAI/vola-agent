---
name: timeout-and-truncation
description: Rules for command timeouts and oversized output when calling run_cmd
tags: cli,run_cmd,optimization
---

When to use: while running commands through `run_cmd`, to keep long runtimes or huge output from blowing up the context.

Steps:

1. Give every `run_cmd` a sensible timeout (default 60s; for jobs you expect to take longer pass `timeout_secs` explicitly, <= 600).
2. If the output was truncated (you see `[aborted]`), narrow it down with `grep` / `head` or redirect it to a file and use `read_file`, instead of re-running the whole command.
3. Never re-run a command whose result is already in the context — reuse what you have.
4. The output cap is 6000 characters; beyond that only the head and tail are kept.

Cautions: a non-zero exit code is not a disaster — read stderr, fix, retry. Timeouts must be passed explicitly, never left to the default.
