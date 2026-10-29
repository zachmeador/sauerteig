# Working on Sauerteig

If `AGENTS-PRIVATE.md` exists in the main checkout's root, read it before
starting and follow it. It holds the local developer's environment and
preferences, such as which agents and models to use, local paths, and machine
limits. It is gitignored: never commit it or copy its contents into tracked
files. From a git worktree, it sits next to the shared `.git` directory that
`git rev-parse --git-common-dir` prints.

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
reviews and commits. Never modify saved worlds in the application-data folder or
the main checkout's `.local/`; copy any world you need into your worktree first.
Before finishing, update the stage section with what changed, check results,
measurements, observed behavior, and remaining gaps, marking anything you could
not observe in the running app.
