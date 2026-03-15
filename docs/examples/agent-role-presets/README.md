# Native agent role preset examples

This directory mirrors a `~/.codex/` layout for custom native roles.

- Copy the declarations from [`config.toml`](./config.toml) into `~/.codex/config.toml`.
- Copy the files under [`agents/`](./agents/) into `~/.codex/agents/`.
- Keep using built-in roles `default`, `explorer`, and `worker` directly. They do not need declarations.

Legacy `~/.codex/agents/*.md` files are migration input only. They do not activate roles at runtime. To migrate one, copy its intent into a `[agents.<role>]` declaration plus a `~/.codex/agents/<role>.toml` file.

## Included starter roles

- Built-in `explorer`: specific codebase questions and repository reconnaissance
- Built-in `worker`: bounded execution work when you do not need a more opinionated custom role
- `planner`: planning, decomposition, rollout order, and delegation handoffs
- `implementer`: scoped code changes with explicit ownership
- `reviewer`: bug, regression, and missing-test review
- `debugger`: reproduction, root-cause isolation, and minimal safe fixes
- `docs`: docs and migration writing aligned to current behavior

## Example compositions

Single-agent workflows:

- Codebase question or reconnaissance: spawn built-in `explorer`
- Bounded implementation pass: spawn built-in `worker` or custom `implementer`
- Planning pass: spawn `planner`
- Debugging pass: spawn `debugger`
- Documentation pass: spawn `docs`

Team compositions:

1. Scout and build

```json
{
  "team_id": "scout-build",
  "members": [
    {
      "name": "scout",
      "task": "Find the relevant files, APIs, and constraints",
      "agent_type": "explorer",
      "background": true
    },
    {
      "name": "builder",
      "task": "Implement the approved change in the owned files only",
      "agent_type": "implementer",
      "worktree": true
    }
  ]
}
```

2. Plan, build, review

```json
{
  "team_id": "plan-build-review",
  "members": [
    {
      "name": "planner",
      "task": "Produce the rollout plan and assign file ownership",
      "agent_type": "planner",
      "background": true
    },
    {
      "name": "builder",
      "task": "Ship the approved code changes",
      "agent_type": "implementer",
      "worktree": true
    },
    {
      "name": "reviewer",
      "task": "Review correctness risks and missing tests",
      "agent_type": "reviewer"
    }
  ]
}
```

3. Parallel implementation with integration review

```json
{
  "team_id": "parallel-implementation",
  "members": [
    {
      "name": "planner",
      "task": "Split the work into bounded file sets",
      "agent_type": "planner",
      "background": true
    },
    {
      "name": "worker-a",
      "task": "Implement the API changes in api/* only",
      "agent_type": "implementer",
      "worktree": true
    },
    {
      "name": "worker-b",
      "task": "Implement the UI changes in ui/* only",
      "agent_type": "implementer",
      "worktree": true
    },
    {
      "name": "reviewer",
      "task": "Review integration risks across both diffs",
      "agent_type": "reviewer"
    }
  ]
}
```

These examples are still plain `spawn_team` payloads. There is no separate team-preset DSL.
