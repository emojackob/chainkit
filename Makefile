PYTHON ?= python3
export PYTHONPATH := $(CURDIR)/packages/chain/src:$(CURDIR)/packages/backend/contract-indexer/src

.PHONY: test check python-test python-check rust-test rust-check indexer-help rust-indexer-help

test: python-test rust-test

python-test:
	$(PYTHON) -m unittest discover -s packages/backend/contract-indexer/tests -v

check: python-check rust-check

python-check:
	$(PYTHON) -m compileall -q packages/chain/src packages/backend/contract-indexer/src
	$(MAKE) python-test

rust-test:
	cargo test --workspace --locked

rust-check:
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets --locked -- -D warnings
	$(MAKE) rust-test

indexer-help:
	$(PYTHON) -m chainkit_indexer --help

rust-indexer-help:
	cargo run --locked -p chainkit-indexer-cli -- --help
