#!/usr/bin/env python3
"""Probe loopback with the configured Proxy Host; never follow redirects."""
import http.client
import os
import sys
from urllib.parse import urlsplit


def main():
    try:
        url = urlsplit(os.environ.get("TREER_PROXY_PUBLIC_URL", "http://proxy.treer.localhost:8080/"))
        if url.scheme not in ("http", "https") or not url.hostname or url.username or url.password:
            return 1
        connection = http.client.HTTPConnection("127.0.0.1", 8080, timeout=2)
        connection.request("GET", "/api/health", headers={"Host": url.netloc})
        response = connection.getresponse()
        response.read()
        connection.close()
        return 0 if response.status == 200 else 1
    except Exception:
        return 1


if __name__ == "__main__":
    sys.exit(main())
