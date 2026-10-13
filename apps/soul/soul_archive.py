"""Bound the complete decoded tar stream, including hidden PAX/GNU headers."""
import bz2
import gzip
import io
import lzma
from pathlib import Path


def archive_mode(source, limit):
    opener = (lambda: Path(source).open('rb')) if isinstance(source, (str, Path)) else (lambda: io.BytesIO(source))
    with opener() as raw:
        prefix = raw.read(6)
        raw.seek(0)
        if prefix.startswith(b'\x1f\x8b'):
            mode, stream = 'r:gz', gzip.GzipFile(fileobj=raw)
        elif prefix.startswith(b'BZh'):
            mode, stream = 'r:bz2', bz2.BZ2File(raw)
        elif prefix.startswith(b'\xfd7zXZ\x00'):
            mode, stream = 'r:xz', lzma.LZMAFile(raw)
        else:
            # Do not let tarfile auto-detect additional compression after preflight.
            mode, stream = 'r:', raw
        total = 0
        try:
            while True:
                data = stream.read(min(65536, limit + 1 - total))
                if not data:
                    return mode
                total += len(data)
                if total > limit:
                    raise ValueError('Decoded archive exceeds total stream limit')
        finally:
            if stream is not raw:
                stream.close()
