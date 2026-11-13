PYTHON ?= python3
export PYTHONPATH := $(CURDIR)/packages/chain/src:$(CURDIR)/packages/backend/contract-indexer/src

.PHONY: test check indexer-help

test:
	$(PYTHON) -m unittest discover -s packages/backend/contract-indexer/tests -v

check:
	$(PYTHON) -m compileall -q packages/chain/src packages/backend/contract-indexer/src
	$(MAKE) test

indexer-help:
	$(PYTHON) -m chainkit_indexer --help
