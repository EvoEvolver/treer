import base64, hashlib, http.server, json, os, socket, sys, urllib.parse, urllib.request

class App(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    def do_GET(self):
        if self.path.startswith('/proxy?'):
            service = urllib.parse.parse_qs(urllib.parse.urlsplit(self.path).query)['service'][0]
            result = json.load(urllib.request.urlopen('http://' + service + '.treer/identity', timeout=10))
            result['runtime_id'] = os.environ['TREER_AGENT_ID']
            result['proxy'] = os.environ['http_proxy']
            body = json.dumps(result).encode()
            self.send_response(200)
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        if self.path == '/ws' and self.headers.get('Upgrade', '').lower() == 'websocket':
            accept = base64.b64encode(hashlib.sha1((self.headers['Sec-WebSocket-Key'] + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest()).decode()
            self.send_response(101)
            self.send_header('Upgrade', 'websocket')
            self.send_header('Connection', 'Upgrade')
            self.send_header('Sec-WebSocket-Accept', accept)
            self.end_headers()
            head = self.rfile.read(2)
            size = head[1] & 127
            mask = self.rfile.read(4)
            payload = self.rfile.read(size)
            payload = bytes(c ^ mask[i % 4] for i,c in enumerate(payload))
            self.wfile.write(bytes([0x81, len(payload)]) + payload)
            self.wfile.flush()
            self.close_connection = True
            return
        if self.path == '/redirect':
            self.send_response(302)
            self.send_header('Location', '/asset')
            self.send_header('Content-Length', '0')
            self.end_headers()
            return
        body = ('<html><title>Treer Docker E2E</title><body>app-ok:' + self.path + '</body></html>').encode()
        self.send_response(200)
        self.send_header('Content-Type', 'text/html')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def do_POST(self):
        body = self.rfile.read(int(self.headers.get('Content-Length', '0')))
        self.send_response(200)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *_): pass

http.server.ThreadingHTTPServer(('127.0.0.1', int(sys.argv[1])), App).serve_forever()
