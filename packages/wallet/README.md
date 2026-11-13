# wallet

Shared wallet connection and signing capabilities.

## Scope and boundaries

- Support wallet connections, account state, network switching, and signing.
- Depend on chain as needed and separate wallet adapters from presentation.
- Keep application transaction rules and pages outside this module.
- Do not store private keys, seed phrases, or other credentials in the repository.

Supported wallets, frameworks, and runtime environments will be defined based on integration requirements.
