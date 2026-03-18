---
name: skim
description: >-
  Codebase orientation powered by rskim. Use this skill whenever the user
  asks to orient in a codebase, understand a project, explore code structure,
  find relevant files for a task, map the architecture, or asks "how is this
  project organized?" or "what files are relevant to [task]?"
user-invocable: false
context: fork
agent: Skimmer
---

Orient in this codebase for the following task:

TASK_DESCRIPTION: $ARGUMENTS

If no task description is provided, perform a general codebase orientation —
map project structure, key modules, and entry points.

Follow the full Skimmer workflow: detect project type, map source directories,
skim for structure, search for task-relevant code, identify integration points,
and generate the orientation report.
