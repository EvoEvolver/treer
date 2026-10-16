# Sourced by the release pipeline scripts. A lap timer for release receipts:
# `lap <stage>` appends "<stage><TAB><seconds since the previous lap>" to
# $TREER_TIMINGS_FILE (no-op when unset). Laps are sequential, so the stages sum
# to the pipeline wall time. Timing only observes; it never changes a result.
lap() {
    [ -n "${TREER_TIMINGS_FILE:-}" ] || return 0
    lap_now=$(date +%s)
    lap_last=$lap_now
    if [ -f "$TREER_TIMINGS_FILE.last" ]; then
        lap_last=$(cat "$TREER_TIMINGS_FILE.last")
    fi
    printf '%s\t%s\n' "$1" "$((lap_now - lap_last))" >> "$TREER_TIMINGS_FILE"
    printf '%s\n' "$lap_now" > "$TREER_TIMINGS_FILE.last"
}
