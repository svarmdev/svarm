# Contributing to Svarm

Thanks for taking the time to contribute.

## Before you open a pull request

For bugs that can be reproduced reliably, please start with a GitHub issue.
Describe how to reproduce the problem, what you expected to happen, and what
actually happened. Include logs, screenshots, or terminal output when they
make the behavior easier to understand. A well-described issue is often the
quickest way to get the problem resolved.

Pull requests are particularly helpful when the behavior depends on an
environment that may not be available to the maintainer. Examples include
macOS, less common shells or terminal emulators, unusual filesystems, SSH
configurations, coding-agent installations, and other machine-specific
settings. In those cases, a PR can preserve both the observed behavior and the
constraints of the environment where it occurs.

## Pull requests

Pull requests are welcome. Please keep them focused and, when practical, add a
test covering any non-trivial behavior.

A submitted PR should generally be viewed as a proposal, reference, or starting
point rather than a guarantee that the exact code will be merged. This project
uses code generation extensively, and generated code can look convincing while
still hiding problems with correctness, lifecycle handling, architecture, or
long-term maintenance.

I am not trying to claim that maintainer-written code is automatically
better than contributor-written code. It is simply about ownership.

Strong pull requests usually contain:

- a concise explanation of the problem and the proposed solution;
- a minimal reproduction or failing test, if one is available;
- relevant edge cases and tradeoffs;
- a small, independently reviewable set of changes; and
- useful logs, screenshots, terminal traces, or benchmarks.
