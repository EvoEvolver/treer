#!/usr/bin/env python3
"""Compare a published bundle, allowing only the Worker's CSP nonce insertion."""
import argparse
from email.parser import Parser
from html.parser import HTMLParser
import ipaddress
from pathlib import Path
import re
import subprocess
import tempfile
import urllib.parse


def expected_html(candidate, csp):
    directives = [part.strip().split() for part in csp.split(';') if part.strip()]
    scripts = [part for part in directives if part[0].lower() == 'script-src']
    if any(part[0].lower() == 'script-src-elem' for part in directives):
        raise ValueError('unexpected script-src-elem override')
    if len(scripts) != 1 or len(scripts[0]) != 2:
        raise ValueError('expected exactly one nonce-only script-src directive')
    match = re.fullmatch(r"'nonce-([A-Za-z0-9+/]{32})'", scripts[0][1])
    if not match:
        raise ValueError('invalid Worker nonce')
    nonce = match[1]
    text = candidate.decode('utf-8')
    # HTMLParser.getpos() counts LF, not every Unicode splitlines separator.
    offsets = [0] + [match.end() for match in re.finditer('\n', text)]
    positions = []

    class Scripts(HTMLParser):
        def handle_starttag(self, tag, attrs):
            if tag != 'script':
                return
            if any(name == 'nonce' for name, _ in attrs):
                raise ValueError('candidate already has a script nonce')
            raw = self.get_starttag_text()
            if raw.endswith('/>'):
                raise ValueError('self-closing script is outside the Worker contract')
            line, column = self.getpos()
            positions.append(offsets[line - 1] + column + len(raw) - 1)

        def handle_startendtag(self, tag, attrs):
            if tag == 'script':
                raise ValueError('self-closing script is outside the Worker contract')

    parser = Scripts(convert_charrefs=False)
    parser.feed(text)
    parser.close()
    if not positions:
        raise ValueError('candidate contains no script tags')
    for position in reversed(positions):
        text = text[:position] + f' nonce="{nonce}"' + text[position:]
    return text.encode('utf-8')


def verify(candidate, served, csp):
    if expected_html(candidate, csp) != served:
        raise ValueError('served frontend differs beyond the permitted CSP nonce insertion')


def verify_url(candidate, url):
    parsed = urllib.parse.urlsplit(url)
    loopback = parsed.hostname == 'localhost'
    try:
        loopback |= ipaddress.ip_address(parsed.hostname or '').is_loopback
    except ValueError:
        pass
    if parsed.username or parsed.password or parsed.query or parsed.fragment or not (
        parsed.scheme == 'https' or (parsed.scheme == 'http' and loopback)
    ):
        raise ValueError('expected an exact HTTPS URL (loopback HTTP for fixtures only)')
    # Reuse the release pipeline's curl transport. Public response headers remain
    # in memory; no cookies, credentials, or redirects are supplied or persisted.
    with tempfile.TemporaryDirectory(prefix='treer-served-frontend-') as temporary:
        body = Path(temporary) / 'index.html'
        result = subprocess.run(['curl', '--disable', '--fail', '--silent', '--show-error', '--max-time', '30',
                                 '--max-filesize', str(16 * 1024 * 1024), '--dump-header', '-',
                                 '--output', str(body), url], capture_output=True, check=True)
        blocks = [block for block in re.split(r'\r?\n\r?\n', result.stdout.decode('iso-8859-1'))
                  if block.startswith('HTTP/')]
        if not blocks:
            raise ValueError('missing HTTP response headers')
        status, fields = blocks[-1].split('\n', 1)
        headers = Parser().parsestr(fields)
        if status.split()[1] != '200' or headers.get_content_type() != 'text/html':
            raise ValueError('expected a successful HTML response; redirects are refused')
        policies = headers.get_all('content-security-policy', [])
        if len(policies) != 1:
            raise ValueError('expected exactly one Content-Security-Policy header')
        served = body.read_bytes()
        if len(served) > 16 * 1024 * 1024:
            raise ValueError('frontend exceeds verification size limit')
        verify(candidate, served, policies[0])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('candidate', type=Path)
    parser.add_argument('url')
    args = parser.parse_args()
    verify_url(args.candidate.read_bytes(), args.url)
    print('Verified served frontend: exact candidate bytes plus matching CSP script nonces')


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        raise SystemExit(f'Frontend verification failed: {error}')
