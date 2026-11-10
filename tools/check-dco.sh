#!/bin/sh
# SPDX-FileCopyrightText: 2026 Omit OS contributors
# SPDX-License-Identifier: MIT OR Apache-2.0
#
# Checks that every commit in a range carries a Developer Certificate of
# Origin sign-off: a "Signed-off-by:" line naming the commit's author.
#
#     tools/check-dco.sh BASE HEAD
#
# Commits made before sign-offs were required (those reachable from the
# commit named in tools/dco-baseline) are not checked.
set -eu
base="$1"
head="$2"
baseline="$(cat "$(dirname "$0")/dco-baseline")"
failed=0
for commit in $(git rev-list "$base..$head" --not "$baseline"); do
    author="$(git log -1 --format='%an <%ae>' "$commit")"
    if ! git log -1 --format=%B "$commit" | grep -qxF "Signed-off-by: $author"; then
        echo "commit $(git log -1 --format='%h %s' "$commit")" >&2
        echo "  has no 'Signed-off-by: $author' line" >&2
        failed=1
    fi
done
if [ "$failed" -ne 0 ]; then
    echo "See CONTRIBUTING.md: commit with 'git commit -s'." >&2
    exit 1
fi
echo "all commits are signed off"
