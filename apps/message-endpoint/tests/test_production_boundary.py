#!/usr/bin/env python3
"""Verify the ordinary hub executable has no cfg(test) Bot origin seam."""
from pathlib import Path
import sys
binary=Path(sys.argv[1])
needles=[b'TREER_TEST_ONLY_BOT_ORIGIN_159',b'with_test_origin_159',b'fixture_account_159',b'https://api.telegram.org']
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
assert not found.intersection(needles[:-1]),'test-only origin/factory leaked into ordinary hub binary'
print('PASS: ordinary hub binary contains fixed Telegram origin and no test-only origin/factory marker or symbol')
