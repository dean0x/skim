#!/usr/bin/env bash
# Large bash script fixture for benchmark testing.
# Approximately 1000 lines with diverse bash constructs.

set -euo pipefail

# ============================================================================
# Constants and configuration
# ============================================================================

readonly SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly LOG_DIR="${LOG_DIR:-/var/log/app}"
readonly CONFIG_FILE="${CONFIG_FILE:-/etc/app/config.env}"
readonly MAX_RETRIES=5
readonly RETRY_DELAY=2
readonly TIMEOUT=300
readonly VERSION="2.0.0"

# ============================================================================
# Logging utilities
# ============================================================================

log_info() {
    local message="$1"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] [INFO]  $message" >&2
}

log_warn() {
    local message="$1"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] [WARN]  $message" >&2
}

log_error() {
    local message="$1"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] [ERROR] $message" >&2
}

log_debug() {
    local message="$1"
    if [[ "${DEBUG:-0}" == "1" ]]; then
        echo "[$(date '+%Y-%m-%d %H:%M:%S')] [DEBUG] $message" >&2
    fi
}

# ============================================================================
# Utility functions
# ============================================================================

require_cmd() {
    local cmd="$1"
    if ! command -v "$cmd" &>/dev/null; then
        log_error "Required command not found: $cmd"
        exit 1
    fi
}

require_env() {
    local var="$1"
    if [[ -z "${!var:-}" ]]; then
        log_error "Required environment variable not set: $var"
        exit 1
    fi
}

is_root() {
    [[ "$(id -u)" -eq 0 ]]
}

is_macos() {
    [[ "$(uname -s)" == "Darwin" ]]
}

is_linux() {
    [[ "$(uname -s)" == "Linux" ]]
}

get_timestamp() {
    date '+%Y%m%d_%H%M%S'
}

trim() {
    local var="$1"
    var="${var#"${var%%[![:space:]]*}"}"
    var="${var%"${var##*[![:space:]]}"}"
    echo "$var"
}

to_upper() {
    echo "${1^^}"
}

to_lower() {
    echo "${1,,}"
}

# ============================================================================
# Retry logic
# ============================================================================

retry() {
    local retries="$1"
    local delay="$2"
    shift 2
    local n=0
    until [[ "$n" -ge "$retries" ]]; do
        "$@" && return 0
        n=$((n + 1))
        log_warn "Attempt $n/$retries failed — retrying in ${delay}s"
        sleep "$delay"
    done
    log_error "Command failed after $retries attempts: $*"
    return 1
}

retry_with_backoff() {
    local retries="$1"
    shift
    local n=0
    local delay=1
    until [[ "$n" -ge "$retries" ]]; do
        "$@" && return 0
        n=$((n + 1))
        log_warn "Attempt $n/$retries failed — retrying in ${delay}s (exponential backoff)"
        sleep "$delay"
        delay=$((delay * 2))
    done
    log_error "Command failed after $retries attempts: $*"
    return 1
}

# ============================================================================
# File and directory helpers
# ============================================================================

ensure_dir() {
    local dir="$1"
    if [[ ! -d "$dir" ]]; then
        mkdir -p "$dir"
        log_debug "Created directory: $dir"
    fi
}

safe_copy() {
    local src="$1"
    local dst="$2"
    if [[ ! -f "$src" ]]; then
        log_error "Source file not found: $src"
        return 1
    fi
    cp -f "$src" "$dst"
    log_debug "Copied $src -> $dst"
}

safe_move() {
    local src="$1"
    local dst="$2"
    if [[ ! -f "$src" ]]; then
        log_error "Source file not found: $src"
        return 1
    fi
    mv -f "$src" "$dst"
    log_debug "Moved $src -> $dst"
}

atomic_write() {
    local path="$1"
    local content="$2"
    local tmp="${path}.tmp.$$"
    echo "$content" > "$tmp"
    mv -f "$tmp" "$path"
}

file_exists() {
    [[ -f "$1" ]]
}

dir_exists() {
    [[ -d "$1" ]]
}

is_readable() {
    [[ -r "$1" ]]
}

is_writable() {
    [[ -w "$1" ]]
}

# ============================================================================
# String utilities
# ============================================================================

str_contains() {
    local haystack="$1"
    local needle="$2"
    [[ "$haystack" == *"$needle"* ]]
}

str_starts_with() {
    local str="$1"
    local prefix="$2"
    [[ "$str" == "${prefix}"* ]]
}

str_ends_with() {
    local str="$1"
    local suffix="$2"
    [[ "$str" == *"${suffix}" ]]
}

str_length() {
    echo "${#1}"
}

str_repeat() {
    local str="$1"
    local count="$2"
    local result=""
    for ((i = 0; i < count; i++)); do
        result+="$str"
    done
    echo "$result"
}

# ============================================================================
# Array utilities
# ============================================================================

array_contains() {
    local needle="$1"
    shift
    local item
    for item in "$@"; do
        [[ "$item" == "$needle" ]] && return 0
    done
    return 1
}

array_join() {
    local sep="$1"
    shift
    local result=""
    local first=1
    for item in "$@"; do
        if [[ "$first" -eq 1 ]]; then
            result="$item"
            first=0
        else
            result+="${sep}${item}"
        fi
    done
    echo "$result"
}

array_length() {
    echo "$#"
}

# ============================================================================
# Network utilities
# ============================================================================

wait_for_port() {
    local host="$1"
    local port="$2"
    local timeout="${3:-30}"
    local elapsed=0
    while ! nc -z "$host" "$port" 2>/dev/null; do
        if [[ "$elapsed" -ge "$timeout" ]]; then
            log_error "Timed out waiting for $host:$port"
            return 1
        fi
        sleep 1
        elapsed=$((elapsed + 1))
    done
    log_debug "Port $host:$port is ready"
}

http_get() {
    local url="$1"
    local output="${2:-/dev/stdout}"
    curl -fsSL --max-time 30 -o "$output" "$url"
}

http_post() {
    local url="$1"
    local data="$2"
    local content_type="${3:-application/json}"
    curl -fsSL -X POST \
        -H "Content-Type: $content_type" \
        -d "$data" \
        "$url"
}

check_connectivity() {
    local host="${1:-8.8.8.8}"
    ping -c 1 -W 2 "$host" &>/dev/null
}

# ============================================================================
# Process management
# ============================================================================

pid_alive() {
    local pid="$1"
    kill -0 "$pid" 2>/dev/null
}

wait_for_pid() {
    local pid="$1"
    local timeout="${2:-60}"
    local elapsed=0
    while pid_alive "$pid"; do
        if [[ "$elapsed" -ge "$timeout" ]]; then
            log_warn "Process $pid still alive after ${timeout}s"
            return 1
        fi
        sleep 1
        elapsed=$((elapsed + 1))
    done
}

kill_gracefully() {
    local pid="$1"
    local grace="${2:-10}"
    if ! pid_alive "$pid"; then
        return 0
    fi
    kill -TERM "$pid"
    local elapsed=0
    while pid_alive "$pid"; do
        if [[ "$elapsed" -ge "$grace" ]]; then
            log_warn "Process $pid did not stop gracefully — sending SIGKILL"
            kill -KILL "$pid" 2>/dev/null || true
            return 0
        fi
        sleep 1
        elapsed=$((elapsed + 1))
    done
}

# ============================================================================
# Docker helpers
# ============================================================================

docker_image_exists() {
    local image="$1"
    docker image inspect "$image" &>/dev/null
}

docker_container_running() {
    local name="$1"
    docker container inspect --format '{{.State.Running}}' "$name" 2>/dev/null | grep -q "true"
}

docker_pull_image() {
    local image="$1"
    log_info "Pulling Docker image: $image"
    retry "$MAX_RETRIES" "$RETRY_DELAY" docker pull "$image"
}

docker_run_detached() {
    local name="$1"
    local image="$2"
    shift 2
    docker run -d --name "$name" "$image" "$@"
}

docker_stop_container() {
    local name="$1"
    if docker_container_running "$name"; then
        log_info "Stopping container: $name"
        docker stop "$name"
    fi
}

docker_remove_container() {
    local name="$1"
    docker rm -f "$name" 2>/dev/null || true
}

docker_exec_in_container() {
    local name="$1"
    shift
    docker exec "$name" "$@"
}

# ============================================================================
# Kubernetes helpers
# ============================================================================

kubectl_apply() {
    local manifest="$1"
    log_info "Applying Kubernetes manifest: $manifest"
    kubectl apply -f "$manifest"
}

kubectl_wait_rollout() {
    local deployment="$1"
    local namespace="${2:-default}"
    local timeout="${3:-300}"
    log_info "Waiting for rollout: $deployment in $namespace"
    kubectl rollout status "deployment/$deployment" \
        --namespace="$namespace" \
        --timeout="${timeout}s"
}

kubectl_get_pods() {
    local namespace="${1:-default}"
    kubectl get pods --namespace="$namespace"
}

kubectl_exec() {
    local pod="$1"
    local namespace="${2:-default}"
    shift 2
    kubectl exec "$pod" --namespace="$namespace" -- "$@"
}

kubectl_scale() {
    local deployment="$1"
    local replicas="$2"
    local namespace="${3:-default}"
    log_info "Scaling $deployment to $replicas replicas"
    kubectl scale "deployment/$deployment" \
        --replicas="$replicas" \
        --namespace="$namespace"
}

kubectl_delete_resource() {
    local resource="$1"
    local name="$2"
    local namespace="${3:-default}"
    kubectl delete "$resource" "$name" \
        --namespace="$namespace" \
        --ignore-not-found=true
}

# ============================================================================
# Git helpers
# ============================================================================

git_current_branch() {
    git rev-parse --abbrev-ref HEAD
}

git_current_sha() {
    git rev-parse HEAD
}

git_short_sha() {
    git rev-parse --short HEAD
}

git_is_clean() {
    git diff --quiet && git diff --cached --quiet
}

git_has_tag() {
    local tag="$1"
    git tag | grep -q "^${tag}$"
}

git_tag_exists_remote() {
    local tag="$1"
    git ls-remote --tags origin "refs/tags/$tag" | grep -q "$tag"
}

git_create_tag() {
    local tag="$1"
    local message="${2:-Release $tag}"
    git tag -a "$tag" -m "$message"
    git push origin "$tag"
}

git_clone_shallow() {
    local url="$1"
    local dir="$2"
    local depth="${3:-1}"
    git clone --depth="$depth" "$url" "$dir"
}

# ============================================================================
# Config loading
# ============================================================================

load_config() {
    local config_file="${1:-$CONFIG_FILE}"
    if [[ -f "$config_file" ]]; then
        # shellcheck source=/dev/null
        source "$config_file"
        log_debug "Loaded config: $config_file"
    else
        log_warn "Config file not found: $config_file"
    fi
}

get_config_value() {
    local key="$1"
    local default="${2:-}"
    local value="${!key:-$default}"
    echo "$value"
}

validate_config() {
    local required_vars=("$@")
    local missing=()
    for var in "${required_vars[@]}"; do
        if [[ -z "${!var:-}" ]]; then
            missing+=("$var")
        fi
    done
    if [[ "${#missing[@]}" -gt 0 ]]; then
        log_error "Missing required config vars: ${missing[*]}"
        return 1
    fi
}

# ============================================================================
# Health checks
# ============================================================================

health_check_http() {
    local url="$1"
    local expected_status="${2:-200}"
    local status
    status=$(curl -o /dev/null -s -w "%{http_code}" "$url")
    [[ "$status" == "$expected_status" ]]
}

health_check_tcp() {
    local host="$1"
    local port="$2"
    nc -z -w 5 "$host" "$port" 2>/dev/null
}

health_check_database() {
    local host="$1"
    local port="${2:-5432}"
    local user="${3:-postgres}"
    PGPASSWORD="${DB_PASSWORD:-}" psql -h "$host" -p "$port" -U "$user" \
        -c "SELECT 1" &>/dev/null
}

run_health_checks() {
    local app_url="${1:-http://localhost:8080/health}"
    log_info "Running health checks..."
    local checks_passed=0
    local checks_failed=0

    if health_check_http "$app_url"; then
        log_info "HTTP health check: PASS"
        checks_passed=$((checks_passed + 1))
    else
        log_error "HTTP health check: FAIL"
        checks_failed=$((checks_failed + 1))
    fi

    log_info "Health checks complete: $checks_passed passed, $checks_failed failed"
    return "$checks_failed"
}

# ============================================================================
# Deployment functions
# ============================================================================

pre_deploy_checks() {
    local env="$1"
    local version="$2"
    log_info "Running pre-deployment checks (env=$env, version=$version)"

    require_cmd docker
    require_cmd kubectl
    require_cmd curl

    if ! git_is_clean; then
        log_warn "Working tree is not clean"
    fi

    log_info "Pre-deployment checks passed"
}

build_docker_image() {
    local tag="$1"
    local dockerfile="${2:-Dockerfile}"
    local context="${3:-.}"
    log_info "Building Docker image: $tag"
    docker build -f "$dockerfile" -t "$tag" "$context"
}

push_docker_image() {
    local tag="$1"
    log_info "Pushing Docker image: $tag"
    retry "$MAX_RETRIES" "$RETRY_DELAY" docker push "$tag"
}

deploy_to_kubernetes() {
    local deployment="$1"
    local image="$2"
    local namespace="${3:-default}"
    log_info "Deploying $deployment with image $image to namespace $namespace"
    kubectl set image "deployment/$deployment" \
        "${deployment}=${image}" \
        --namespace="$namespace"
    kubectl_wait_rollout "$deployment" "$namespace"
}

rollback_deployment() {
    local deployment="$1"
    local namespace="${2:-default}"
    log_warn "Rolling back deployment: $deployment in $namespace"
    kubectl rollout undo "deployment/$deployment" --namespace="$namespace"
    kubectl_wait_rollout "$deployment" "$namespace"
}

# ============================================================================
# Notification helpers
# ============================================================================

send_slack_message() {
    local webhook_url="$1"
    local message="$2"
    local channel="${3:-#deployments}"
    local payload
    payload=$(cat <<EOF
{
    "channel": "$channel",
    "text": "$message"
}
EOF
)
    http_post "$webhook_url" "$payload"
}

send_pagerduty_alert() {
    local routing_key="$1"
    local summary="$2"
    local severity="${3:-warning}"
    local payload
    payload=$(cat <<EOF
{
    "routing_key": "$routing_key",
    "event_action": "trigger",
    "payload": {
        "summary": "$summary",
        "severity": "$severity",
        "source": "$(hostname)"
    }
}
EOF
)
    http_post "https://events.pagerduty.com/v2/enqueue" "$payload" \
        "application/json"
}

notify_deploy_start() {
    local env="$1"
    local version="$2"
    log_info "Notifying deploy start: env=$env version=$version"
    if [[ -n "${SLACK_WEBHOOK_URL:-}" ]]; then
        send_slack_message "$SLACK_WEBHOOK_URL" \
            "Deploy started: $version to $env"
    fi
}

notify_deploy_success() {
    local env="$1"
    local version="$2"
    log_info "Notifying deploy success"
    if [[ -n "${SLACK_WEBHOOK_URL:-}" ]]; then
        send_slack_message "$SLACK_WEBHOOK_URL" \
            "Deploy succeeded: $version to $env"
    fi
}

notify_deploy_failure() {
    local env="$1"
    local version="$2"
    local reason="${3:-unknown error}"
    log_error "Deploy failed: env=$env version=$version reason=$reason"
    if [[ -n "${SLACK_WEBHOOK_URL:-}" ]]; then
        send_slack_message "$SLACK_WEBHOOK_URL" \
            "DEPLOY FAILED: $version to $env — $reason"
    fi
    if [[ -n "${PAGERDUTY_ROUTING_KEY:-}" ]]; then
        send_pagerduty_alert "$PAGERDUTY_ROUTING_KEY" \
            "Deploy failed: $version to $env" "critical"
    fi
}

# ============================================================================
# Cleanup and teardown
# ============================================================================

cleanup_old_images() {
    local prefix="$1"
    local keep="${2:-5}"
    log_info "Cleaning up old Docker images (keeping last $keep for prefix: $prefix)"
    docker images --format "{{.Repository}}:{{.Tag}} {{.CreatedAt}}" \
        | grep "^${prefix}" \
        | sort -k2 -r \
        | tail -n +"$((keep + 1))" \
        | awk '{print $1}' \
        | xargs -r docker rmi || true
}

cleanup_temp_files() {
    local pattern="${1:-/tmp/app_*}"
    log_debug "Cleaning up temp files: $pattern"
    # shellcheck disable=SC2086
    rm -f $pattern 2>/dev/null || true
}

cleanup_on_exit() {
    local exit_code="$?"
    if [[ "$exit_code" -ne 0 ]]; then
        log_error "Script exited with code $exit_code"
        cleanup_temp_files
    fi
}

setup_cleanup_trap() {
    trap cleanup_on_exit EXIT
    trap 'log_error "Interrupted"; exit 130' INT TERM
}

# ============================================================================
# Argument parsing
# ============================================================================

show_usage() {
    cat <<EOF
Usage: $(basename "$0") [OPTIONS] <environment> <version>

Options:
  -h, --help        Show this help message
  -d, --debug       Enable debug logging
  -f, --force       Skip pre-deployment checks
  -n, --dry-run     Print actions without executing
  --namespace NS    Kubernetes namespace (default: default)
  --timeout SEC     Rollout timeout in seconds (default: 300)

Arguments:
  environment       Target environment (staging, production)
  version           Application version to deploy

Examples:
  $(basename "$0") staging v1.2.3
  $(basename "$0") --debug production v2.0.0
  $(basename "$0") --dry-run staging latest
EOF
}

parse_args() {
    local -n args_ref="$1"
    shift

    args_ref[debug]="0"
    args_ref[force]="0"
    args_ref[dry_run]="0"
    args_ref[namespace]="default"
    args_ref[timeout]="300"
    args_ref[env]=""
    args_ref[version]=""

    while [[ "$#" -gt 0 ]]; do
        case "$1" in
            -h|--help)
                show_usage
                exit 0
                ;;
            -d|--debug)
                args_ref[debug]="1"
                export DEBUG=1
                ;;
            -f|--force)
                args_ref[force]="1"
                ;;
            -n|--dry-run)
                args_ref[dry_run]="1"
                ;;
            --namespace)
                args_ref[namespace]="$2"
                shift
                ;;
            --timeout)
                args_ref[timeout]="$2"
                shift
                ;;
            -*)
                log_error "Unknown option: $1"
                show_usage
                exit 1
                ;;
            *)
                if [[ -z "${args_ref[env]}" ]]; then
                    args_ref[env]="$1"
                elif [[ -z "${args_ref[version]}" ]]; then
                    args_ref[version]="$1"
                else
                    log_error "Unexpected argument: $1"
                    show_usage
                    exit 1
                fi
                ;;
        esac
        shift
    done

    if [[ -z "${args_ref[env]}" ]] || [[ -z "${args_ref[version]}" ]]; then
        log_error "Missing required arguments: environment and version"
        show_usage
        exit 1
    fi
}

# ============================================================================
# Validation
# ============================================================================

validate_environment() {
    local env="$1"
    local valid_envs=("staging" "production" "development" "testing")
    if ! array_contains "$env" "${valid_envs[@]}"; then
        log_error "Invalid environment: $env (valid: ${valid_envs[*]})"
        return 1
    fi
}

validate_version() {
    local version="$1"
    if [[ "$version" == "latest" ]]; then
        return 0
    fi
    if ! [[ "$version" =~ ^v?[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9._-]+)?$ ]]; then
        log_error "Invalid version format: $version (expected: v1.2.3 or 1.2.3)"
        return 1
    fi
}

validate_namespace() {
    local namespace="$1"
    if ! kubectl get namespace "$namespace" &>/dev/null; then
        log_error "Kubernetes namespace does not exist: $namespace"
        return 1
    fi
}

# ============================================================================
# Main deployment orchestrator
# ============================================================================

run_deploy() {
    local env="$1"
    local version="$2"
    local namespace="${3:-default}"
    local dry_run="${4:-0}"

    log_info "Starting deployment: version=$version env=$env namespace=$namespace"

    if [[ "$dry_run" == "1" ]]; then
        log_info "[DRY RUN] Would deploy $version to $env"
        return 0
    fi

    local image="myapp:${version}"
    local deployment="myapp-${env}"

    notify_deploy_start "$env" "$version"

    if ! deploy_to_kubernetes "$deployment" "$image" "$namespace"; then
        notify_deploy_failure "$env" "$version" "kubectl rollout failed"
        rollback_deployment "$deployment" "$namespace"
        return 1
    fi

    if ! run_health_checks "http://${env}.example.com/health"; then
        notify_deploy_failure "$env" "$version" "health checks failed post-deploy"
        rollback_deployment "$deployment" "$namespace"
        return 1
    fi

    notify_deploy_success "$env" "$version"
    log_info "Deployment complete: $version to $env"
}

# ============================================================================
# Main entry point
# ============================================================================

main() {
    setup_cleanup_trap
    load_config

    declare -A args
    parse_args args "$@"

    if [[ "${args[debug]}" == "1" ]]; then
        export DEBUG=1
        log_debug "Debug mode enabled"
    fi

    validate_environment "${args[env]}"
    validate_version "${args[version]}"

    if [[ "${args[force]}" == "0" ]]; then
        pre_deploy_checks "${args[env]}" "${args[version]}"
    fi

    run_deploy \
        "${args[env]}" \
        "${args[version]}" \
        "${args[namespace]}" \
        "${args[dry_run]}"
}

main "$@"
