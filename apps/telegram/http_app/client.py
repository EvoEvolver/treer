#!/usr/bin/env python3
"""Tiny external client: client.py BASE_URL TOKEN_FILE inbox|outbox|ack < request.json.

The token is never an argument value or environment variable. Responses go to stdout.
This deliberately does not pair: obtain the write-once token using the private-file
curl flow in README.md. Use HTTPS except in isolated local tests.
"""
import os
import stat
import sys
import urllib.request
import urllib.parse

class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs): return None

def main():
    if len(sys.argv) != 4: raise ValueError()
    base, path, operation = sys.argv[1:]
    url = urllib.parse.urlsplit(base)
    if url.scheme != 'https' or not url.hostname or url.username or url.password or url.query or url.fragment:
        raise ValueError()
    flags = os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK
    fd = os.open(path, flags)
    with os.fdopen(fd, 'r', encoding='ascii') as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077 or info.st_size > 128:
            raise ValueError()
        token = stream.read(128).strip()
    if len(token) != 43 or not all(c.isascii() and (c.isalnum() or c in '_-') for c in token): raise ValueError()
    routes = {'inbox':('POST','v1/inbox'), 'outbox':('GET','v1/outbox?timeout=30'), 'ack':('POST','v1/outbox/ack')}
    method, route = routes[operation]
    data = None if method == 'GET' else sys.stdin.buffer.read(65537)
    if data is not None and len(data) > 65536: raise ValueError()
    headers = {'Authorization':'Bearer '+token, 'Content-Type':'application/json'}
    request = urllib.request.Request(base.rstrip('/')+'/'+route, data=data, headers=headers, method=method)
    with urllib.request.build_opener(NoRedirect()).open(request, timeout=35) as response:
        sys.stdout.buffer.write(response.read(512000)+b'\n')

if __name__ == '__main__':
    try: main()
    except Exception:
        # urllib errors can contain URLs; no request headers/credentials are logged.
        raise SystemExit('HTTP client failed; check URL, private token file, operation and endpoint availability') from None
