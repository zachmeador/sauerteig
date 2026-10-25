# Working on Sauerteig

Implementation work runs in Claude Code subagents on Sonnet 5.5 by default, or in
Codex sessions on `gpt-6-astra` with `max` reasoning, unless the user requests a
different model. (`gpt-6.1-sol` is not available to this account in Codex.)

Read [project tracking](docs/tracking/README.md) before starting development.
Keep its overview and the relevant work document current as scope, implementation,
or evidence changes. Follow the completion and cleanup rules there; a passing
test, successful build, or loadable map is not proof of a complete user workflow.

Use the regular documentation for current behavior and technical contracts.
Keep unfinished work in `docs/tracking/` rather than duplicate backlogs elsewhere.
Delete obsolete tracking documents after moving any still-useful information to
its canonical home. Git history preserves previous states.

## Implementation sessions

Work orders are the stage sections of tracking documents. Work only in the
worktree and branch you were given, and stay inside the stage's scope and
parallel-work boundaries. Do not commit, push, or switch branches; the design lead
reviews and commits. Never modify the maintainer's saved worlds under the
application-data folder, `_old-games-user-data/`, or the main checkout's `.local/`;
copy any world you need into your worktree first. Before finishing, update the
stage section with what changed, check results, measurements, observed behavior,
and remaining gaps, marking anything you could not observe in the running app.
