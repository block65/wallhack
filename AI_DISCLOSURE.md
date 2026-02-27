# AI Disclosure

This project uses generative AI throughout the development process. This
document exists for transparency.

## How AI is used

AI handles a lot of implementation, refactoring, research, and mechanical work —
the stuff that used to take longer for no good reason. It's code generation on
steroids, made possible by the fact that software engineering spent decades
building the infrastructure to support exactly this: version control, automated
testing, CI/CD, static analysis, type systems. The automation harness was
already there. AI just stepped into it.

The human role is ideation, judgment, architecture, true reasoning, domain
decisions, and final arbiter of quality. AI produces code fast. Producing
*correct, secure, well-reasoned* code still requires a human who knows what
they're looking at.

AI slop is a skill issue, not an AI issue.

## How quality is maintained

Every change passes through the same process regardless of origin:

- Unit tests, `clippy` with pedantic warnings, formatting checks, and benchmarks
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

The code is open source. Read it, test it, benchmark it, break it. Judge it on
its merits.