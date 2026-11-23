---
name: skill-library
description: How this agent organizes, writes, and maintains its own skills. Read before creating, updating, or retiring any skill.
tags: skills,meta,self-learning
---

# Skill Library Manager

This file is the manager page for the whole skill library. It is not a domain skill — it defines the
convention every other skill follows, and it is what the agent reads before writing a new one.

## Layout

```
agent/skills/
├── SKILL.md            <- this manager page
└── references/
    ├── timeout-and-truncation.md
    └── ...             <- one Markdown file per skill
```

- `SKILL.md` (here) documents the rules and stays small.
- `references/` holds the actual skills. `skill_save` always writes into `references/`.
- The Markdown files are the source of truth. The SQLite `skills` table is only an index/cache and is rebuilt
  from the files on startup, so never hand-edit the database.

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

## Steps

## Pitfalls
```

- `name` is English kebab-case and unique across the library; it is also what the index shows.
- `description` answers "when should the agent load this", phrased as a trigger, not as a summary of contents.
- `tags` are optional, comma separated, used for grouping.
- The body must be executable: exact commands, flags, paths, expected output, and the failure modes you hit.
  A skill that reads like prose is useless; a skill that reads like a checklist is useful.

## When to write a skill

Write one when, and only when, all of these hold:

1. The knowledge is reusable — it will plausibly be needed again.
2. You actually verified it in a real run, not from memory.
3. It is not already covered by an existing skill in `references/` (search the index first; prefer updating an
   existing file over adding a near-duplicate).

Typical triggers: a build/run recipe for this machine, a tool-specific command sequence, a repeated pitfall with
its fix, a non-obvious API or environment quirk.

Do not write a skill for: one-off task progress, temporary state, generic knowledge that needs no verification,
or anything the user asked you not to persist.

## How to add or update

1. Search the index (or `references/`) for an existing skill on the topic.
2. If it exists, `skill_view` it and extend the body; keep the name stable so links and memory referencess survive.
3. If it does not exist, `skill_save` with a new kebab-case name, a trigger description, and tags.
4. Keep the body ordered: when to use → prerequisites → steps → verification → pitfalls.
5. After adding, re-read the index in the system prompt; if the list grew stale, the file is the thing to fix —
   the index is regenerated from files, never the other way around.

## Retiring a skill

- Delete the file from `references/` when the procedure is obsolete (dead tool, replaced workflow), then note the
  replacement in the skill that supersedes it.
- Never rename a skill casually: memory entries and session summaries may point at the old name.
