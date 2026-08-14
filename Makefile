lint:
	cargo fmt --all
	cargo clippy --fix --allow-dirty --all-targets --all-features -- --deny warnings

tui:
	cargo build --release --package pi-agent-tui --bin pi-agent

tui-headless:
	cargo run --package pi-agent-tui --bin pi-agent-headless -- $(ARGS)

TUI_SMOKE_MODEL ?= openrouter/free

tui-smoke:
	set -a; . ./.env; set +a; cargo run --package pi-agent-tui --bin pi-agent-headless -- --model $(TUI_SMOKE_MODEL) --prompt 'Reply with exactly READY and no additional text. Do not call any tools.'

quality-fast:
	PYTHONDONTWRITEBYTECODE=1 python3 -m evals.quality fast

quality-resources:
	PYTHONDONTWRITEBYTECODE=1 python3 -m evals.quality resources
