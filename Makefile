.DEFAULT_GOAL := build

CARGO ?= cargo

.PHONY: build release run check test fmt clippy

build:
	$(CARGO) build --locked -p kadr-app

release:
	$(CARGO) build --locked --release -p kadr-app

run:
	$(CARGO) run --locked -p kadr-app

check:
	$(CARGO) check --locked --workspace --all-targets

test:
	$(CARGO) test --locked --workspace

fmt:
	$(CARGO) fmt --all -- --check

clippy:
	$(CARGO) clippy --locked --workspace --all-targets -- -D warnings
