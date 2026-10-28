# Same-origin checksums detect corruption, not a compromised publisher.
# Validate the entire staged set before chmod, replacement, or execution.
if command -v sha256sum >/dev/null 2>&1; then
    checksum() { sha256sum "$1"; }
elif command -v shasum >/dev/null 2>&1; then
    checksum() { shasum -a 256 "$1"; }
else
    echo "treer: sha256sum or shasum is required" >&2
    exit 1
fi
for binary in treer treer-launch treer-agent-host treer-agent-server; do
    expected=$(awk -v binary="$binary" '
        $2 == binary || $2 == "*" binary {
            if (NF != 2 || length($1) != 64 || $1 !~ /^[[:xdigit:]]+$/) exit 1
            print tolower($1)
            count++
        }
        END { if (count != 1) exit 1 }
    ' "$tmp_dir/SHA256SUMS") || {
        echo "treer: missing, duplicate, or invalid SHA256SUMS entry for $binary" >&2
        exit 1
    }
    actual=$(checksum "$tmp_dir/$binary" | awk '{print $1}')
    if [ "$actual" != "$expected" ]; then
        echo "treer: SHA256 mismatch for $binary" >&2
        exit 1
    fi
done
