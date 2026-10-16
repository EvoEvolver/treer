# Sourced by test-canary.sh. Cookies stay in shell memory, never in a curl jar.
# The Proxy rejects cookie-authenticated writes without the exact Console
# Origin, so callers set canary_console_origin (scheme://host[:port] of the
# configured Console URL; never guessed from the Proxy URL).
canary_login_cookies() {
    login_url=$1
    login_headers=$(curl -fsS -D - -o /dev/null \
        -H 'Content-Type: application/json' \
        ${canary_console_origin:+-H "Origin: $canary_console_origin"} \
        --data-binary @- "$login_url") || return 1
    login_cookie=$(printf '%s\n' "$login_headers" | awk '
        tolower($0) ~ /^set-cookie:/ {
            sub(/^[^:]*:[[:space:]]*/, ""); sub(/;.*/, ""); sub(/\r$/, "")
            if (cookies != "") cookies = cookies "; "
            cookies = cookies $0
        }
        END { print cookies }
    ')
    [ -n "$login_cookie" ] || { echo "login returned no session cookie" >&2; return 1; }
    printf '%s\n' "$login_cookie"
}
