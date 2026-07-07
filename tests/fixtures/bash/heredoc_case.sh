#!/usr/bin/env bash
# Script with heredocs, case statements, and loops

PLATFORM="$(uname -s)"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/myapp"

# Generate a default config file using a heredoc
write_default_config() {
    local output_path="$1"
    cat > "$output_path" <<EOF
# myapp default configuration
log_level = info
max_connections = 100
timeout_seconds = 30
EOF
    echo "Wrote default config to $output_path"
}

# Install platform-specific dependencies
install_deps() {
    case "$PLATFORM" in
        Linux)
            apt-get install -y curl jq git
            ;;
        Darwin)
            brew install curl jq git
            ;;
        *)
            echo "Unsupported platform: $PLATFORM" >&2
            exit 1
            ;;
    esac
}

# Wait for a port to become available
wait_for_port() {
    local host="$1"
    local port="$2"
    local timeout="${3:-60}"
    local elapsed=0

    while ! nc -z "$host" "$port" 2>/dev/null; do
        if [ "$elapsed" -ge "$timeout" ]; then
            echo "Timed out waiting for $host:$port" >&2
            return 1
        fi
        sleep 1
        elapsed=$((elapsed + 1))
    done
    echo "Port $host:$port is ready"
}

mkdir -p "$CONFIG_DIR"
write_default_config "$CONFIG_DIR/config.toml"
install_deps
wait_for_port localhost 8080
