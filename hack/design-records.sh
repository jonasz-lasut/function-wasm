#!/usr/bin/env bash
# design-records.sh holds the two places that restate a design record's
# status to the record itself. Every docs/one-pager-*.md carries its own
# `* Status:` line (docs/project/design-records.md says what it may hold;
# the revision there moves whenever the design changes), and two other
# files repeat it: the design records index, docs/project/design-records.md,
# one row per record, and the book's table of contents, docs/SUMMARY.md,
# whose entry for a record puts the status in parentheses after the title.
# Both have drifted more than once as records were revised, so `make vale`
# runs this check beside the prose lint. A row or an entry out of step, a
# record with no row, or a row or an entry pointing at a record that no
# longer exists is a failure naming the file and what it should say.
#
# The comparison takes the status up to its first parenthesis (the
# revision's note stays in the record) and ignores case (the index and the
# contents capitalise the status; the record keeps it lowercase for Vale's
# colon rules). The contents may list a record outside the design records
# list without a status, as the use-case examples page is listed among the
# project pages; such an entry is held to the title alone.
set -euo pipefail
cd "$(dirname "$0")/.."

index=docs/project/design-records.md
contents=docs/SUMMARY.md
failed=0

drift() {
	echo "design-records: $*" >&2
	failed=1
}

lower() {
	printf '%s' "$1" | tr '[:upper:]' '[:lower:]'
}

for record in docs/one-pager-*.md; do
	file=${record#docs/}
	title=$(sed -n '1s/^# //p' "$record")
	if [ -z "$title" ]; then
		drift "$record: the first line is not the record's H1"
		continue
	fi
	status=$(grep -m1 '^\* Status: ' "$record" | sed -e 's/^\* Status: //' -e 's/ *(.*$//' -e 's/ *$//' || true)
	if [ -z "$status" ]; then
		drift "$record: no '* Status:' line"
		continue
	fi

	# The index: `| [Title](../one-pager-x.md) | Status |`.
	rows=$(grep -F -- "](../$file)" "$index" || true)
	case $(printf '%s' "$rows" | grep -c .) in
	0) drift "$index: no row for $file; expected '| [$title](../$file) | $status |'" ;;
	1)
		row_title=$(printf '%s' "$rows" | sed -e 's/^| \[//' -e 's/\](\.\..*$//')
		row_status=$(printf '%s' "$rows" | sed -e 's/^.*) | //' -e 's/ |$//')
		[ "$row_title" = "$title" ] || drift "$index: the row for $file is titled '$row_title'; the record is titled '$title'"
		[ "$(lower "$row_status")" = "$(lower "$status")" ] || drift "$index: the row for $file says '$row_status'; the record says '$status'"
		;;
	*) drift "$index: $file has more than one row" ;;
	esac

	# The contents: `- [Title (Status)](one-pager-x.md)`, or `- [Title](one-pager-x.md)`
	# outside the design records list.
	entries=$(grep -F -- "]($file)" "$contents" || true)
	case $(printf '%s' "$entries" | grep -c .) in
	0) drift "$contents: no entry for $file; expected '- [$title ($status)]($file)'" ;;
	1)
		text=$(printf '%s' "$entries" | sed -e 's/^ *- \[//' -e 's/\]([^)]*)$//')
		case $text in
		*')')
			entry_title=${text% (*}
			entry_status=${text##* (}
			entry_status=${entry_status%)}
			[ "$(lower "$entry_status")" = "$(lower "$status")" ] || drift "$contents: the entry for $file says '$entry_status'; the record says '$status'"
			;;
		*) entry_title=$text ;;
		esac
		[ "$entry_title" = "$title" ] || drift "$contents: the entry for $file is titled '$entry_title'; the record is titled '$title'"
		;;
	*) drift "$contents: $file has more than one entry" ;;
	esac
done

# A row or an entry left behind by a record that was renamed or removed.
for linked in $(grep -oh 'one-pager-[A-Za-z0-9-]*\.md' "$index" "$contents" | sort -u); do
	[ -f "docs/$linked" ] || drift "$index or $contents links $linked, which does not exist under docs/"
done

if [ "$failed" -ne 0 ]; then
	exit 1
fi
echo "design-records: the index and the contents match every record"
