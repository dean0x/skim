#!/usr/bin/env bash
# Script with multiple functions demonstrating structure mode

set -euo pipefail

# Configuration
readonly MAX_RETRIES=3
readonly LOG_FILE="/var/log/deploy.log"

# Log a message with timestamp
log() {
    local level="$1"
    local message="$2"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] [$level] $message" | tee -a "$LOG_FILE"
}

# Check if a required command is available
require_cmd() {
    local cmd="$1"
    if ! command -v "$cmd" &>/dev/null; then
        log "ERROR" "Required command '$cmd' not found"
        exit 1
    fi
}

# Retry a command up to MAX_RETRIES times
retry() {
    local n=0
    local cmd=("$@")
    until [ "$n" -ge "$MAX_RETRIES" ]; do
        "${cmd[@]}" && break
        n=$((n + 1))
        log "WARN" "Attempt $n/$MAX_RETRIES failed, retrying..."
        sleep 2
    done
    if [ "$n" -ge "$MAX_RETRIES" ]; then
        log "ERROR" "Command failed after $MAX_RETRIES attempts: ${cmd[*]}"
        return 1
    fi
}

# Deploy the application
deploy() {
    local env="$1"
    local version="$2"

    log "INFO" "Deploying version $version to $env"
    require_cmd docker
    require_cmd kubectl

    retry docker pull "myapp:$version"
    kubectl set image deployment/myapp "myapp=myapp:$version" --namespace="$env"
    kubectl rollout status deployment/myapp --namespace="$env"
    log "INFO" "Deployment complete"
}

# Main entry point
main() {
    local env="${1:-staging}"
    local version="${2:-latest}"
    deploy "$env" "$version"
}

main "$@"
