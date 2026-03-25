#!/usr/bin/env bash
set -euo pipefail

SOURCE_URL="https://raw.githubusercontent.com/pradt2/always-online-stun/master/valid_hosts.txt"

usage() {
    cat <<'EOF'
Generate Rust STUN server constant from always-online-stun.

Usage:
  scripts/generate_stun_servers.sh [--output <path>] [--input <path>]

Options:
  --output <path>  Write generated Rust to file instead of stdout.
  --input <path>   Read hosts from a local file instead of fetching URL.
EOF
}

output_path=""
input_path=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --output)
            output_path="${2:-}"
            shift 2
            ;;
        --input)
            input_path="${2:-}"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "Unknown argument: $1" >&2
            usage >&2
            exit 1
            ;;
    esac
done

if [[ -n "$input_path" ]]; then
    if [[ ! -f "$input_path" ]]; then
        echo "Input file not found: $input_path" >&2
        exit 1
    fi
    hosts="$(cat "$input_path")"
else
    hosts="$(curl -fsSL "$SOURCE_URL")"
fi

clean_hosts="$(
    printf '%s\n' "$hosts" \
        | sed -e 's/\r$//' -e 's/#.*$//' \
        | awk 'NF'
)"

count="$(printf '%s\n' "$clean_hosts" | awk 'END { print NR }')"

if [[ "$count" -eq 0 ]]; then
    echo "No hosts found after filtering input." >&2
    exit 1
fi

generated="$(
    {
        printf 'const STUN_SERVERS: [&str; %s] = [\n' "$count"
        printf '%s\n' "$clean_hosts" | awk '{ printf "    \"%s\",\n", $0 }'
        printf '];\n'
    }
)"

if [[ -n "$output_path" ]]; then
    printf '%s' "$generated" > "$output_path"
else
    printf '%s' "$generated"
fi
