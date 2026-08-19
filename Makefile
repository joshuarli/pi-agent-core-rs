.PHONY: lint tui tui-headless tui-smoke local-install local-model local-server local quality-fast quality-resources

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

OMLX_ROOT ?= $(HOME)/d/omlx
OMLX_VENV ?= $(OMLX_ROOT)/.venv
OMLX_PYTHON_VERSION ?= 3.13
OMLX_PYTHON ?= $(OMLX_VENV)/bin/python
OMLX_BIN ?= $(OMLX_VENV)/bin/omlx
OMLX_HF ?= $(OMLX_VENV)/bin/hf
LOCAL_PORT ?= 12345
LOCAL_BASE_URL ?= http://127.0.0.1:$(LOCAL_PORT)/v1
LOCAL_MODEL ?= Qwen3.5-4B-MLX-4bit
LOCAL_MODEL_REPO ?= mlx-community/Qwen3.5-4B-MLX-4bit
LOCAL_MODEL_DIR ?= $(HOME)/.omlx/models/$(LOCAL_MODEL)
LOCAL_OMLX_BASE_PATH ?= /tmp/pi-agent-omlx-$(LOCAL_PORT)
LOCAL_OMLX_LOG ?= $(LOCAL_OMLX_BASE_PATH)/server.log
LOCAL_PI_ARGS ?=

# Keep the source checkout runnable without a separate manual Python setup. Both
# commands are safe to repeat: uv preserves an existing environment and only
# updates the editable install or dependencies that changed.
local-install:
	@command -v uv >/dev/null 2>&1 || { echo "missing required command: uv" >&2; exit 1; }
	@test -d "$(OMLX_ROOT)" || { echo "missing oMLX checkout: $(OMLX_ROOT)" >&2; exit 1; }
	@uv venv --allow-existing --python "$(OMLX_PYTHON_VERSION)" "$(OMLX_VENV)"
	@uv pip install --python "$(OMLX_PYTHON)" --editable "$(OMLX_ROOT)"

# The legacy huggingface-cli wrapper shipped by newer huggingface_hub releases exits with a
# deprecation error; `hf` is the working CLI in the same oMLX virtual environment.
local-model: local-install
	@test -x "$(OMLX_HF)" || { echo "missing oMLX Hugging Face CLI: $(OMLX_HF)" >&2; exit 1; }
	@mkdir -p "$(dir $(LOCAL_MODEL_DIR))"
	@echo "Ensuring $(LOCAL_MODEL_REPO) is present at $(LOCAL_MODEL_DIR)"
	@"$(OMLX_HF)" download --local-dir "$(LOCAL_MODEL_DIR)" "$(LOCAL_MODEL_REPO)"

local-server: local-model
	@test -x "$(OMLX_BIN)" || { echo "missing oMLX executable: $(OMLX_BIN)" >&2; exit 1; }
	@if curl -fsS --max-time 1 "$(LOCAL_BASE_URL)/models" 2>/dev/null | grep -Fq '"id":"$(LOCAL_MODEL)"'; then \
		echo "oMLX already serving $(LOCAL_MODEL) at $(LOCAL_BASE_URL)"; \
	else \
		if curl -sS --max-time 1 "$(LOCAL_BASE_URL)/models" >/dev/null 2>&1; then \
			echo "$(LOCAL_BASE_URL) is already occupied by a different service" >&2; \
			exit 1; \
		fi; \
		mkdir -p "$(LOCAL_OMLX_BASE_PATH)"; \
		echo "Starting oMLX on $(LOCAL_BASE_URL)"; \
		nohup "$(OMLX_BIN)" serve --base-path "$(LOCAL_OMLX_BASE_PATH)" --model-dir "$(HOME)/.omlx/models" --no-hf-cache --host 127.0.0.1 --port "$(LOCAL_PORT)" >"$(LOCAL_OMLX_LOG)" 2>&1 </dev/null & \
		ready=0; \
		for attempt in $$(seq 1 60); do \
			if curl -fsS --max-time 1 "$(LOCAL_BASE_URL)/models" 2>/dev/null | grep -Fq '"id":"$(LOCAL_MODEL)"'; then ready=1; break; fi; \
			sleep 1; \
		done; \
		if [ "$$ready" -ne 1 ]; then \
			echo "oMLX did not become ready; see $(LOCAL_OMLX_LOG)" >&2; \
			tail -n 80 "$(LOCAL_OMLX_LOG)" >&2 2>/dev/null || true; \
			exit 1; \
		fi; \
		echo "oMLX ready at $(LOCAL_BASE_URL)"; \
	fi

local: local-server
	cargo build --package pi-agent-tui --bin pi-agent
	"$(CURDIR)/target/debug/pi-agent" --provider local --local-base-url "$(LOCAL_BASE_URL)" --model "$(LOCAL_MODEL)" --thinking low $(LOCAL_PI_ARGS)

quality-fast:
	PYTHONDONTWRITEBYTECODE=1 python3 -m evals.quality fast

quality-resources:
	PYTHONDONTWRITEBYTECODE=1 python3 -m evals.quality resources
