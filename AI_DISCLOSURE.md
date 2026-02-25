# AI Disclosure

This project uses a generative AI-assisted development process. Here's what that
means.

## How AI is used

AI is a development tool in the same way an IDE, a compiler, linter, or debugger
is a tool. Hell, even `rustc` and `clippy` write code for you.

In this project, AI assists with implementation, refactoring, research, and the
kind of tedious mechanical work that would otherwise just take longer for no
good reason.

It's worth clarifying however that many of the architectural decisions, design
choices, and code reviews are human-driven.

The development workflow is codified, structured and deliberate:

- Specific guidelines have been created to define project conventions, naming
  rules, and quality standards that AI must follow
- All changes go through the same review process - unit tests, clippy with
  pedantic warnings, formatting checks, benchmarks, and human review BEFORE pull
  requests or merging
- Feature branches, pull requests, and branch protection apply equally
  regardless of who or what authored a commit
- Commit history is kept clean and meaningful - not a stream-of-consciousness
  log of AI prompts
- Direct explicit dependencies are manually vetted before inclusion

The workflow also involves a manual 3-phase process of planning, implementation
and review. This is not "vibe-coding" and "type a sentence and ship whatever
comes out."

It's basically pair programming with a very fast, very literal colleague who
never gets bored of renaming things and doesn't take lunch breaks. Followed by a
pair-review with another colleague who has their own individual quirks, but is
just as thorough.

## Why this disclosure exists

Transparency.

Some people have strong opinions about AI's role in software development,
particularly in open source products. And that's fine. You are entitled to know
how this project is built so you can make your own informed decisions about
using it.

The code is open source. Clone it. Read it. Run the tests. Run the benchmarks.
Check the commit history. Judge the work on its merits, not its authors.

## Concerns

If your concern is code quality or behaviour - great! Please file an issue.
Quality matters here regardless of how the code was written.

If your concern is philosophical - this file exists so you have all the facts.

You're welcome. Enjoy! 
