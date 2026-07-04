#!/usr/bin/env bash
# Script testing CRLF tolerance and dialect noise

# Array operations
ITEMS=("alpha" "beta" "gamma")
declare -A SETTINGS

SETTINGS[host]="localhost"
SETTINGS[port]="8080"
SETTINGS[debug]="false"

# Process each item with a for loop
process_items() {
    local -a items=("$@")
    local result=0
    for item in "${items[@]}"; do
        echo "Processing: $item"
        result=$((result + 1))
    done
    return "$result"
}

# Arithmetic and string operations
compute_hash() {
    local input="$1"
    echo -n "$input" | sha256sum | awk '{print $1}'
}

# Trap for cleanup
cleanup() {
    echo "Cleaning up temporary files"
    rm -f /tmp/myapp_*.tmp
}
trap cleanup EXIT

process_items "${ITEMS[@]}"
echo "Host: ${SETTINGS[host]}:${SETTINGS[port]}"
