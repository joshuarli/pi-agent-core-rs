#!/bin/sh
# Capture clean default-minimal fx frames without Bun or a live provider.
# The output is raw terminal-owned text/grid evidence; style and cursor paint
# are intentionally not inferred from this capture path.
set -eu

fx_bin=${FX_BIN:-/Users/josh/d/fx/zig-out/bin/fx}
output_dir=${1:-${TMPDIR:-/tmp}/tea-fx-minimal-captures}

if [ ! -x "$fx_bin" ]; then
  echo "fx binary is not executable: $fx_bin (run: (cd /Users/josh/d/fx && zig build))" >&2
  exit 2
fi
command -v tmux >/dev/null 2>&1 || {
  echo "tmux is required" >&2
  exit 2
}

mkdir -p "$output_dir"
binary_sha256=$(shasum -a 256 "$fx_bin" | awk '{print $1}')

capture_state() {
  columns=$1
  rows=$2
  state=$3
  input=$4
  state_dir="$output_dir/${state}-${columns}x${rows}"
  mkdir -p "$state_dir"

  home_dir=$(mktemp -d "${TMPDIR:-/tmp}/tea-fx-home.XXXXXX")
  mkdir -p "$home_dir/.fx"
  printf '%s\n' '{"maxxing_mode":"minimal"}' > "$home_dir/.fx/settings.json"
  socket="tea-fx-${$}-${columns}-${rows}-${state}"
  session="tea-fx-${$}-${columns}-${rows}-${state}"

  cleanup() {
    tmux -L "$socket" -f /dev/null kill-server >/dev/null 2>&1 || true
    rm -rf "$home_dir"
  }
  trap cleanup EXIT HUP INT TERM

  tmux -L "$socket" -f /dev/null new-session -d -s "$session" -x "$columns" -y "$rows" \
    /bin/zsh -lc "env HOME='$home_dir' FX_DISABLE_KEYCHAIN=1 FX_SKIP_ONBOARDING=1 '$fx_bin'"

  ready=0
  attempt=0
  while [ "$attempt" -lt 100 ]; do
    pane=$(tmux -L "$socket" -f /dev/null capture-pane -p -t "$session" 2>/dev/null || true)
    case "$pane" in
      *"Run /help for commands"*) ready=1; break ;;
    esac
    attempt=$((attempt + 1))
    sleep 0.1
  done
  if [ "$ready" -ne 1 ]; then
    echo "fx did not reach the composer at ${columns}x${rows}" >&2
    exit 1
  fi

  tmux -L "$socket" -f /dev/null capture-pane -p -t "$session" -S - > "$state_dir/grid.txt"
  tmux -L "$socket" -f /dev/null display-message -p -t "$session" \
    '#{pane_width} #{pane_height} #{cursor_x} #{cursor_y}' > "$state_dir/cursor.txt"
  printf '%s\n' "$input" > "$state_dir/input.txt"

  if [ "$state" = "help" ]; then
    tmux -L "$socket" -f /dev/null send-keys -t "$session" -l '/help'
    ready=0
    attempt=0
    while [ "$attempt" -lt 100 ]; do
      pane=$(tmux -L "$socket" -f /dev/null capture-pane -p -t "$session" 2>/dev/null || true)
      case "$pane" in
        *"┃ /help"*) ready=1; break ;;
      esac
      attempt=$((attempt + 1))
      sleep 0.1
    done
    if [ "$ready" -ne 1 ]; then
      echo "fx did not accept /help at ${columns}x${rows}" >&2
      exit 1
    fi
    tmux -L "$socket" -f /dev/null send-keys -t "$session" Enter
    marker="Commands 38"
  else
    tmux -L "$socket" -f /dev/null send-keys -t "$session" -l '/'
    marker="Results 41"
  fi
  ready=0
  attempt=0
  while [ "$attempt" -lt 100 ]; do
    pane=$(tmux -L "$socket" -f /dev/null capture-pane -p -t "$session" 2>/dev/null || true)
    case "$pane" in
      *"$marker"*) ready=1; break ;;
    esac
    attempt=$((attempt + 1))
    sleep 0.1
  done
  if [ "$state" != "startup" ] && [ "$ready" -ne 1 ]; then
    echo "fx did not reach $state at ${columns}x${rows}" >&2
    exit 1
  fi

  if [ "$state" != "startup" ]; then
    tmux -L "$socket" -f /dev/null capture-pane -p -t "$session" -S - > "$state_dir/grid.txt"
    tmux -L "$socket" -f /dev/null display-message -p -t "$session" \
      '#{pane_width} #{pane_height} #{cursor_x} #{cursor_y}' > "$state_dir/cursor.txt"
  fi
  tmux -L "$socket" -f /dev/null send-keys -t "$session" C-c >/dev/null 2>&1 || true
  cleanup
  trap - EXIT HUP INT TERM
}

for size in 80x24 120x40; do
  columns=${size%x*}
  rows=${size#*x}
  capture_state "$columns" "$rows" startup "startup (no input)"
  capture_state "$columns" "$rows" help "/help + Enter"
  capture_state "$columns" "$rows" slash-menu "/"
done

printf 'captured default-minimal fx frames in %s\n' "$output_dir"
printf 'fx binary sha256: %s\n' "$binary_sha256"
