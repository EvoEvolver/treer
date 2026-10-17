#!/usr/bin/env python3
"""Disposable Managed-App identity smoke, NOT the hub/relay implementation.

Release runs this only in canary-e2e under a temporary App. No bot/transport.
Inherits that App's own identity for CLI calls; never reads credential contents.
Only synthetic self-messages are sent/acked. Logs/HTTP contain check names only.
"""
import argparse
import json
import os
import re
from pathlib import Path
import selectors
import subprocess
import time
from http.server import BaseHTTPRequestHandler, HTTPServer


FAILURES = frozenset({
    'invalid_smoke_config', 'cli_unavailable', 'cli_timeout', 'cli_output_limit',
    'cli_rejected', 'cli_invalid_json', 'idempotency_mismatch', 'receive_mismatch',
    'sender_mismatch', 'redelivery_missing', 'ack_missing', 'ack_not_applied',
})
STEPS = frozenset({'configuration', 'send', 'idempotency', 'receive',
                   'redelivery', 'ack', 'ack_replay', 'final_receive'})


class SmokeError(Exception):
    def __init__(self, code, step='configuration'):
        # Only these fixed labels may cross the health boundary.
        self.code = code if code in FAILURES else 'cli_invalid_json'
        self.step = step if step in STEPS else 'configuration'
        super().__init__(self.code)



def cli(executable, arguments, body=None, timeout=15):
    try:
        process = subprocess.Popen([executable, *arguments], stdin=subprocess.PIPE,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    except OSError:
        raise SmokeError('cli_unavailable') from None
    selector = selectors.DefaultSelector()
    output = bytearray()
    total = 0
    try:
        if body is not None:
            encoded = json.dumps(body).encode()
            if len(encoded) > 4096:
                raise SmokeError('invalid_smoke_config')
            process.stdin.write(encoded)
        process.stdin.close()
        selector.register(process.stdout, selectors.EVENT_READ, True)
        selector.register(process.stderr, selectors.EVENT_READ, False)
        deadline = time.monotonic() + timeout
        while selector.get_map():
            if time.monotonic() >= deadline:
                raise SmokeError('cli_timeout')
            for key, _ in selector.select(min(0.1, max(0, deadline - time.monotonic()))):
                chunk = os.read(key.fd, 65536)
                if not chunk:
                    selector.unregister(key.fileobj)
                    continue
                total += len(chunk)
                if total > 1024 * 1024:
                    raise SmokeError('cli_output_limit')
                if key.data:
                    output.extend(chunk)
        code = process.wait(timeout=max(0.01, deadline - time.monotonic()))
        if code:
            raise SmokeError('cli_rejected')
        result = json.loads(output)
        if not isinstance(result, dict):
            raise SmokeError('cli_invalid_json')
        return result
    except (OSError, ValueError, subprocess.TimeoutExpired):
        raise SmokeError('cli_unavailable') from None
    finally:
        selector.close()
        if process.poll() is None:
            process.kill()  # Only the exact child started above.
        process.wait()
        for stream in (process.stdin, process.stdout, process.stderr):
            stream.close()


def exercise(executable, own_id, nonce):
    step = 'configuration'
    try:
        if not re.fullmatch(r'[a-z]+_[A-Za-z0-9_-]{8,128}', own_id) or not nonce.isalnum() or not 8 <= len(nonce) <= 48:
            raise SmokeError('invalid_smoke_config')
        step = 'send'
        body = 'synthetic app-runtime smoke ' + nonce
        key = 'app-runtime-smoke-' + nonce
        arguments = ['message', 'send', '--to', own_id, '--idempotency-key', key, '--stdin-envelope']
        first = cli(executable, arguments, {'body': body})['message']['message_id']
        step = 'idempotency'
        second = cli(executable, arguments, {'body': body})['message']['message_id']
        if first != second:
            raise SmokeError('idempotency_mismatch')
        step = 'receive'
        deliveries = cli(executable, ['message', 'receive', '--wait', '1000', '--limit', '10'])['deliveries']
        matching = [d for d in deliveries if d['message']['message_id'] == first]
        if len(matching) != 1 or matching[0]['message']['body'] != body:
            raise SmokeError('receive_mismatch')
        if matching[0]['message']['sender']['id'] != own_id:
            raise SmokeError('sender_mismatch')
        delivery = matching[0]['delivery_id']
        # Check redelivery BEFORE ack without touching any unrelated delivery.
        step = 'redelivery'
        again = cli(executable, ['message', 'receive', '--wait', '0', '--limit', '10'])['deliveries']
        if not any(d['delivery_id'] == delivery for d in again):
            raise SmokeError('redelivery_missing')
        ack = ['message', 'ack', delivery, '--operation-id', key + '-ack']
        step = 'ack'
        result = cli(executable, ack)
        if delivery not in result['acknowledged_delivery_ids'] + result['already_acknowledged_delivery_ids']:
            raise SmokeError('ack_missing')
        step = 'ack_replay'
        cli(executable, ack)  # Replaying the same ack operation is safe.
        step = 'final_receive'
        final = cli(executable, ['message', 'receive', '--wait', '0', '--limit', '10'])['deliveries']
        if any(d['delivery_id'] == delivery for d in final):
            raise SmokeError('ack_not_applied')
        return ['self_send', 'same_key_same_id', 'authenticated_sender',
                'receive_from_app', 'redelivery_before_ack', 'ack_replay', 'no_redelivery_after_ack']
    except SmokeError as error:
        raise SmokeError(error.code, step) from None
    except (KeyError, TypeError, IndexError):
        raise SmokeError('cli_invalid_json', step) from None


def report(executable, own_id, nonce):
    try:
        checks = exercise(executable, own_id, nonce)
        return {'status': 'passed', 'checks': checks}
    except SmokeError as error:
        return {'status': 'failed', 'checks': [], 'failure': error.code, 'step': error.step}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cli', required=True)
    parser.add_argument('--nonce', required=True)
    parser.add_argument('--port', type=int, required=True)
    args = parser.parse_args()
    if not Path(args.cli).is_absolute() or not 1024 <= args.port <= 65535:
        parser.error('absolute CLI path and unprivileged port required')
    # IDs are not credentials. Leave workload credential handling to the CLI.
    health_report = report(args.cli, os.environ.get('TREER_AGENT_ID', ''), args.nonce)

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            if self.path != '/health':
                self.send_error(404)
                return
            data = json.dumps(health_report).encode()
            self.send_response(200 if health_report['status'] == 'passed' else 503)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Cache-Control', 'no-store')
            self.send_header('Content-Length', str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def log_message(self, *args):
            pass

    HTTPServer(('127.0.0.1', args.port), Handler).serve_forever()


if __name__ == '__main__':
    main()
