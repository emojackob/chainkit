CHAINKIT_INDEXER ?= chainkit-indexer
DATABASE ?= chainkit.sqlite
CONFIG ?= indexer-config.json
PROJECT ?= demo
PROJECT_NAME ?= Demo

.PHONY: project-create project-list serve
project-create:
	$(CHAINKIT_INDEXER) --database "$(DATABASE)" create-project "$(PROJECT)" --name "$(PROJECT_NAME)"
project-list:
	$(CHAINKIT_INDEXER) --database "$(DATABASE)" list-projects
serve:
	$(CHAINKIT_INDEXER) --database "$(DATABASE)" serve --config "$(CONFIG)"
