---
description: Codebase orientation powered by rskim - map project structure, find task-relevant code, and generate integration plans
---

# Skim Command

Quickly orient in a codebase using `rskim` to extract structure, find relevant files, and plan integration for a task.

## Usage

```
/skim add JWT authentication
/skim understand the payment flow
/skim
```

## Input

`$ARGUMENTS` contains whatever follows `/skim`:
- Task description: "add JWT authentication"
- Empty: perform general codebase orientation

## Phases

### Phase 1: Parse Input

If `$ARGUMENTS` is non-empty, use it as the task description.
If empty, the task is "General codebase orientation — map project structure, key modules, and entry points."

### Phase 2: Orient

Spawn the Skimmer agent:

```
Task(subagent_type="Skimmer"):
"Orient in this codebase for the following task:

TASK_DESCRIPTION: {task_description}

Follow your full workflow: detect project type, map source directories, skim for structure,
search for task-relevant code, identify integration points, and generate the orientation report."
```

### Phase 3: Present

Present the Skimmer agent's orientation report directly to the user. Do not summarize or truncate — show the full structured report.

If the task description was specific (not general orientation), add a brief "Next Steps" section suggesting which files to modify and in what order.

## Error Handling

If the Skimmer agent reports rskim is unavailable:
1. Show the error to the user
2. Suggest installing: `npm install -g rskim` or `cargo install rskim`
3. Note that the agent fell back to manual exploration and the report is still usable
