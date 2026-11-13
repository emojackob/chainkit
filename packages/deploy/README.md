# deploy

Shared capabilities for contract deployment, upgrades, record keeping, and validation.

## Scope and boundaries

- Provide deployment and upgrade workflows, record formats, and validation.
- Reuse chain as needed and expose deployment operations to tooling.
- Keep addresses, network configuration, deployment records, and upgrade history in application repositories.
- Obtain credentials from the runtime environment; do not commit them.
- Applications select deployment parameters and upgrade targets.

The deployment framework, record formats, and upgrade validation requirements will be defined based on integration requirements.
