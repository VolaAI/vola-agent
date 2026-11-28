---
name: skill-library
description: How this agent organizes, writes, refactors, and retires its own skills. Read before creating, updating, or deleting any skill.
tags: skills,meta,self-learning
---

# Skill Library Manager

This file is the manager page for the whole skill library. It is not a domain skill — it defines the
convention every other skill follows, and it is what the agent reads before writing a new one.

The agent owns this library. It may query it, add to it, rewrite its own skills, and delete skills
that are obsolete. The goal is a small, accurate, executable library — not a growing pile of prose.

## Layout

```
agent/skills/
├── SKILL.md            <- this manager page (protected, never delete)
└── references/
    ├── timeout-and-truncation.md
    └── ...             <- one Markdown file per skill
```

- `SKILL.md` (here) documents the rules and stays small.
- `references/` holds the actual skills. `skill_save` always writes into `references/`.
- The Markdown files are the source of truth. The SQLite `skills` table is only an index/cache and is rebuilt
  from the files on startup, so never hand-edit the database.
- The index in the system prompt is generated from the files every turn; fix the file, not the index.

## File format

Every file in `references/` starts with YAML frontmatter, then a Markdown body:

```markdown
---
name: kebab-case-name
description: when to use this skill, one sentence
tags: comma,separated,topics
---

# Title

## When to use

## Prerequisites

## Steps

## Verification

## Pitfalls
```

- `name` is English kebab-case and unique across the library; it is also what the index shows.
- `description` answers "when should the agent load this", phrased as a trigger, not as a summary of contents.
- `tags` are optional, comma separated, used for grouping.
- The body must be executable: exact commands, flags, paths, expected output, and the failure modes you hit.
  A skill that reads like prose is useless; a skill that reads like a checklist is useful.

## Querying the library

1. The system prompt already lists every skill (name, description, file). Scan it first.
2. `skill_list` gives the same list plus use counts and file paths.
3. `skill_view <name>` reads the full body. Do this before relying on a skill or before changing it.
4. If nothing matches, say so and solve the task directly; do not invent a skill from memory.

## When to add a skill

Write one when, and only when, all of these hold:

1. The knowledge is reusable — it will plausibly be needed again.
2. You actually verified it in a real run, not from memory.
3. It is not already covered by an existing skill (search the index first; prefer updating an existing
   file over adding a near-duplicate).

Typical triggers: a build/run recipe for this machine, a tool-specific command sequence, a repeated
pitfall with its fix, a non-obvious API or environment quirk.

Do not write a skill for: one-off task progress, temporary state, generic knowledge that needs no
verification, or anything the user asked you not to persist. A single command is not a skill.

## Adding or updating a skill

1. Search the index (or `references/`) for an existing skill on the topic.
2. If it exists, `skill_view` it, then `skill_save` the improved full body under the **same name**.
   Keep the name stable so memory entries and session summaries keep pointing at it.
3. If it does not exist, `skill_save` with a new kebab-case name, a trigger description, and tags.
4. Keep the body ordered: when to use → prerequisites → steps → verification → pitfalls.
5. When refactoring, remove stale steps and dead commands instead of appending contradictions; the
   latest body must be internally consistent.
6. After saving, re-read the index in the system prompt; if it looks wrong, the file is the thing to
   fix — the index is regenerated from files, never the other way around.

## Retiring a skill

- Use `skill_delete <name>` when the procedure is obsolete (dead tool, replaced workflow), wrong, or
  fully superseded by another skill.
- Check `skill_list` first and make sure nothing still references it. If another skill depends on it,
  update that skill in the same turn and mention the replacement in its body.
- Never delete `SKILL.md`; it is the manager page and is protected by the tool.
- Never rename a skill casually: memory entries and session summaries may point at the old name. If a
  rename is truly needed, create the new skill, update references, then delete the old one.

## Keeping the library healthy

- Prefer a few dense, verified skills over many thin ones. Merge near-duplicates (~70% overlap) by
  extending the stronger skill and deleting the weaker one.
- If a skill has not been used in a long time and the workflow is gone, propose deleting it; only the
  user decides when the skill encodes user-specific preferences.
- Keep this manager page short. It is read on every turn, so every line costs context.
