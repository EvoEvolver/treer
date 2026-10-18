#!/usr/bin/env python3
"""Verify the ordinary hub executable has no cfg(test) transport/refresh scheduling seam."""
from pathlib import Path
import sys
import unittest

def check(binary):
    needles=[b'TREER_TEST_ONLY_BOT_ORIGIN_159',b'with_test_origin_159',b'fixture_account_159',b'with_test_readback_gate_159',b'test-only resume conflict:',b'test-only readback superseded:',b'https://api.telegram.org']
    found=set()
    with binary.open('rb') as stream:
        tail=b''
        while True:
            chunk=stream.read(1024*1024)
            if not chunk:break
            data=tail+chunk
            for value in needles:
                if value in data:found.add(value)
            tail=data[-128:]
    assert needles[-1] in found,'production binary lost its fixed Bot API origin'
    assert not found.intersection(needles[:-1]),'test-only origin/factory/scheduling diagnostic leaked into ordinary hub binary'
    print('PASS: ordinary hub binary contains fixed Telegram origin and no test-only origin/factory/scheduling diagnostic marker or symbol')

class ProductionBoundary(unittest.TestCase):
    def test_debug_hub_contains_no_test_only_transport_seam(self):
        check(Path(__file__).resolve().parents[3] / 'target/debug/treer-message-hub')

if __name__ == '__main__':
    if len(sys.argv) == 2 and not sys.argv[1].startswith('-'):
        check(Path(sys.argv[1]))
    else:
        unittest.main()
