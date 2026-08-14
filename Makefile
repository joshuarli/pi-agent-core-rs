lint:
	cargo fmt --all
	cargo clippy --fix --allow-dirty --all-targets --all-features -- --deny warnings

quality-fast:
	PYTHONDONTWRITEBYTECODE=1 python3 -m evals.quality fast

quality-resources:
	PYTHONDONTWRITEBYTECODE=1 python3 -m evals.quality resources
