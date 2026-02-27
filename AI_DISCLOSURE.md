# AI Disclosure

This project uses generative AI throughout its development process. This
document exists for transparency.

## How AI is used

In this project, AI handles a fair chunk of implementation, refactoring,
research, and mechanical work — the stuff that used to take a lot longer.

A human still provides ideation, judgment, architecture, true reasoning, domain
decisions, and serves as final arbiter of quality.

## How quality is maintained

Every change passes through the same process regardless of origin:

- Unit tests, linting with pedantic warnings, formatting checks, and benchmarks
- Human review before anything is merged
- Feature branches, pull requests, and branch protection — non-negotiable
- Direct dependencies manually vetted before inclusion
- Clean, intentional commit history

The verification layer doesn't care who authored the code. It checks correctness
either way.

## Why this disclosure exists

Transparency.

Some people have strong opinions about AI in software development, particularly
in open source. You're entitled to know how this project is built so you can
make informed decisions about using it.

The high impact of generative code generation was made possible by the fact that
lots of highly intelligent and hardworking software engineers spent decades
building the infrastructure to support exactly this: version control, automated
testing, CI/CD, static analysis, type systems.

That harness was already there. AI just stepped into it.

AI produces mountains of code, fast. Producing *correct, secure, well-reasoned*
code still requires a human who knows what they're looking at. AI slop is a
skill issue, not an AI issue.

The code is open source. Read it, test it, benchmark it, break it. Judge it on
its merits.