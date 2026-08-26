.PHONY: build run release test test-linux-e2e lint fmt check conformance packaging-check release-readiness clean install release-patch release-minor release-major update-deps

build:
	cargo build

run:
	cargo run

release:
	cargo build --release

test:
	cargo nextest run

test-linux-e2e: build
	scripts/test-linux-e2e.sh

lint:
	cargo fmt -- --check
	cargo clippy --all-targets -- -D warnings

fmt:
	cargo fmt

check: lint test packaging-check

# Score the binary against The CLI Spec (clispec.dev). Requires `clispec`.
conformance: release
	clispec score ./target/release/cacheferret

packaging-check:
	scripts/test-homebrew-formula.sh

release-readiness:
	scripts/check-release-secrets.sh

clean:
	cargo clean

install: release
	mkdir -p ~/.local/bin
	cp target/release/cacheferret ~/.local/bin/cacheferret

update-deps:
	upd --apply --max-bump minor --lang rust,actions

release-patch:
	vership bump patch

release-minor:
	vership bump minor

release-major:
	vership bump major
