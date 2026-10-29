# How to use goals

A **goal** keeps Abacus working on one objective, turn after turn, until it is
done. It is the tool for work one turn doesn't finish, like "make every test
pass", without you sending "keep going" after each turn.

## Your first goal

```text
/goal Implement the CSV importer in src/import.rs and get `cargo test` green
```

That sets the goal and starts the first turn with it. The goal stays pinned
above the composer. Whenever a turn ends by itself while the goal is still
active, Abacus starts the next turn and tells the model the goal isn't done
yet. Files and conversation carry over, so each turn builds on the last.

## When it stops

The goal keeps going until one of these happens:

- **The model finishes it.** Once it has achieved and verified the objective,
  it calls `goal_update` with status `complete`.
- **The model needs you.** When it can't go further without a decision,
  access, or information only you have, it pauses the goal with a note saying
  what it needs.
- **You pause or clear it.** `/goal pause` and `/goal clear` (or the goal bar
  in the desktop app) stop it. `/goal resume` carries on.
- **You stop the turn.** `Esc` in the terminal, or the stop button in the
  desktop app, ends that turn and doesn't start another. The goal stays active,
  so the next turn that ends by itself carries it on again. Pause it to stop
  for good.
- **It stalls.** Two turns in a row that end without calling a single tool
  pause the goal, so a model that keeps answering instead of working can't run
  up turns forever.

A turn that fails (the provider refused it, say) doesn't start another turn
either. Send a message, or `/goal resume` after a pause, to carry on.

## Commands

| Command | Action |
| --- | --- |
| `/goal <objective>` | Set the goal and start working on it |
| `/goal` | Show the goal and its latest note |
| `/goal pause` / `/goal resume` | Hold the goal, or carry on |
| `/goal edit <text>` | Change the objective |
| `/goal done` | Mark it complete yourself |
| `/goal clear` | Remove it |

The goal is saved with the session, so it survives `abacus --continue` and
`--resume`. Messages you send while a goal runs steer the running turn, as they
always do.

## Headless / CI

`--goal` makes the prompt the goal and keeps a headless run going until the
goal is complete or paused:

```sh
abacus -p "Implement the importer and get the whole test suite green" --goal --yolo
```

`--yolo` lets the run carry out every action. Without it the run is in AUTO:
edits inside the workspace and whatever the safety reviewer clears run, and
anything the reviewer can't decide on is skipped, since nobody is there to ask.
With `--output-format json` the result carries `"goal"`, which is where the
goal ended up: `complete`, or `paused`.

## Writing a goal that finishes

- **Make "done" checkable.** "until `cargo test` passes" beats "until it
  works": the model should verify completion, not just claim it.
- **Keep it to one objective.** For a huge task, set a goal per milestone.
- **Say what needs you.** If some step needs your sign-off, write that into the
  goal, and the model pauses there instead of guessing.
