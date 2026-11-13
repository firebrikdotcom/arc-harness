.PHONY: migrate serve check test

migrate:
	cargo run -- migrate

serve:
	cargo run -- serve

check:
	cargo check

test:
	cargo test
	node --test tests/session-directory-live.test.cjs
