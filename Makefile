.PHONY: format lint build test check

format:
	cargo fmt --all -- --check

lint:
	cargo clippy --workspace --all-targets --all-features -- -D warnings

build:
	cargo build --workspace

test:
	cargo test --workspace

check: format lint build test

