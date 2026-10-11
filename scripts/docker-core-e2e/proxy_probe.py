"""Runs as an Agent child: no credential reads or token acquisition."""
import json, os, socket, sys, urllib.parse, urllib.request

mode, target = sys.argv[1:3]
entry = os.environ['http_proxy']
assert entry == os.environ['https_proxy'] == os.environ['TREER_HTTP_PROXY']
if mode == 'http':
    request = urllib.request.Request(target, headers={'X-Treer-Agent-Id': 'spoof', 'X-Treer-Identity-Token': 'spoof'})
    value = json.load(urllib.request.urlopen(request, timeout=15))
    print(json.dumps({'proxy': entry, 'response': value}))
else:
    parsed = urllib.parse.urlsplit(entry)
    sock = socket.create_connection((parsed.hostname, parsed.port), timeout=15)
    if mode == 'connect':
        sock.sendall(('CONNECT ' + target + ' HTTP/1.1\r\nHost: ' + target + '\r\n\r\n').encode())
        header = b''
        while not header.endswith(b'\r\n\r\n'):
            header += sock.recv(1)
        assert header.startswith(b'HTTP/1.1 200'), header
        data = bytes(range(256)) * 4096
        import threading
        def send():
            sock.sendall(data)
            sock.shutdown(socket.SHUT_WR)
        writer = threading.Thread(target=send)
        writer.start()
        received = bytearray()
        while True:
            chunk = sock.recv(65536)
            if not chunk:
                break
            received.extend(chunk)
        writer.join()
        assert received == data
        print(json.dumps({'proxy': entry, 'bytes': len(received)}))
    elif mode == 'ws':
        sock.sendall(('GET ' + target + ' HTTP/1.1\r\nHost: ' + urllib.parse.urlsplit(target).netloc +
            '\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n').encode())
        header = b''
        while not header.endswith(b'\r\n\r\n'):
            header += sock.recv(1)
        assert header.startswith(b'HTTP/1.1 101'), header
        payload, mask = b'proxy-websocket', b'1234'
        sock.sendall(bytes([0x81, 0x80 | len(payload)]) + mask + bytes(c ^ mask[i % 4] for i, c in enumerate(payload)))
        frame = sock.makefile('rb')
        assert frame.read(2) == bytes([0x81, len(payload)])
        assert frame.read(len(payload)) == payload
        print(json.dumps({'proxy': entry, 'websocket': True}))
    sock.close()
