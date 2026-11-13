# tooling

Engineering commands, builds, and Makefile support.

## Scope and boundaries

- Provide engineering tools that applications invoke through versioned dependencies.
- Support builds, checks, and command orchestration to reduce copied scripts.
- Use Makefiles as command entry points and maintain shared execution logic in tools.
- Deployment domain logic belongs in deploy; tooling invokes and orchestrates it.

Command names, argument conventions, distribution methods, and supported environments remain to be defined.
