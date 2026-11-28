---
name: skill-library
description: How this agent organizes, writes, refactors and retires its own skills. Read before creating, updating or deleting any skill.
tags: skills,meta,self-learning
---

# Skill Library Manager

This is the manager page, not a domain skill. It defines the convention every skill follows. The agent owns the library: query it, add to it, rewrite its own skills, delete obsolete ones. Aim for a small, accurate, executable library — not a pile of prose.

## Layout

```
agent/skills/
├── SKILL.md            <- this page (protected, never delete)
└── references/         <- one Markdown file per skill
```

- `skill_save` always writes into `references/`.
- Markdown files are the source of truth; the SQLite `skills` table is only an index, rebuilt from files at startup. Never hand-edit the DB.
- The index in the system prompt is generated from files every turn; fix the file, not the index.

## File format

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

- `name`: English kebab-case, unique; it is what the index shows.
- `description`: the trigger ("when to load this"), not a summary of contents.
- `tags`: optional, comma separated.
- Body: executable checklist — exact commands, flags, paths, expected output, failure modes. Prose is useless.

## Query

1. Scan the skill index in the system prompt first.
2. `skill_list` gives the full list with use counts and paths.
3. `skill_view <name>` reads the body; do this before relying on or changing a skill.
4. If nothing matches, solve the task directly; do not invent a skill from memory.

## Add only when needed

All must hold:
1. Reusable — plausibly needed again.
2. Verified in a real run, not from memory.
3. Not already covered (search the index; prefer updating an existing file over a near-duplicate).

Good triggers: a machine-specific build/run recipe, a tool command sequence, a repeated pitfall with its fix, a non-obvious API/environment quirk.
Never: one-off progress, temporary state, generic knowledge needing no verification, anything the user asked not to persist. A single command is not a skill.

## Add or update

1. Search the index for an existing skill on the topic.
2. If it exists: `skill_view`, then `skill_save` the improved full body under the **same name**. Keep names stable so memory/summaries keep pointing at it.
3. If not: `skill_save` with a new kebab-case name, trigger description and tags.
4. Order the body: when to use → prerequisites → steps → verification → pitfalls.
5. Refactor by removing stale steps and dead commands, not by appending contradictions.
6. After saving, re-read the index; if it looks wrong, fix the file.

## Retire

- `skill_delete <name>` when a skill is obsolete (dead tool, replaced workflow), wrong, or fully superseded.
- Check `skill_list` first; if another skill depends on it, update that skill in the same turn and note the replacement.
- Never delete `SKILL.md` (protected by the tool).
- Do not rename casually: memory/summaries may point at the old name. If needed, create the new skill, update references, then delete the old one.

## Keep it healthy

- Prefer a few dense, verified skills over many thin ones; merge ~70% duplicates by extending the stronger one and deleting the weaker.
- Propose deleting long-unused skills whose workflow is gone; only the user decides when a skill encodes user preferences.
- Keep this page short — it is read on demand, and every line costs context.
