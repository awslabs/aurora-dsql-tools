#!/usr/bin/env bash
# Lists production names that changed between HEAD and the merge-base with
# the target branch. Pure advisory — pasted into PR descriptions to remind
# the author which corpus fixtures may need updating.
#
# Usage: scripts/grammar_diff.sh [base-branch]
set -euo pipefail

base="${1:-main}"
repo_root="$(git rev-parse --show-toplevel)"
ebnf="$repo_root/dsql_grammar.ebnf"

if [[ ! -f "$ebnf" ]]; then
    echo "error: $ebnf not found" >&2
    exit 1
fi

# Suppress only grep's exit-1-on-no-matches; let real failures from
# git diff, grep (exit >= 2), sed, or sort propagate via pipefail.
git diff "$base"... -- "$ebnf" \
    | { grep -E '^[+-][A-Za-z][A-Za-z0-9_]*[[:space:]]*=' || [[ $? == 1 ]]; } \
    | sed -E 's/^([+-])[[:space:]]*([A-Za-z][A-Za-z0-9_]*).*/\1 \2/' \
    | sort -u
