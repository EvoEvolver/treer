import base64, json, os, subprocess, sys, threading, http.server, socketserver, socket, ssl, urllib.request

name = sys.argv[1]
port = int(sys.argv[2])
class Http(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == '/identity':
            token = self.headers.get('X-Treer-Identity-Token', '')
            encoded = token.split('.')[1]
            claims = json.loads(base64.urlsafe_b64decode(encoded + '=' * (-len(encoded) % 4)))
            # Verify the signature through the real Proxy; return claims, never the token.
            request = urllib.request.Request(claims['iss'].rstrip('/') + '/.treer/identity/verify',
                json.dumps({'token': token, 'audience': claims['aud']}).encode(), {'Content-Type': 'application/json'})
            verified = json.load(urllib.request.urlopen(request, timeout=10))
            assert verified['active'], verified
            result = {'claims': verified['claims'], 'bearer_matches': self.headers.get('Authorization') == 'Bearer ' + token,
                'leaked': any(self.headers.get(key) for key in ['X-Treer-Agent-Id', 'X-Treer-Workload-Credential', 'X-Treer-Operator-Credential', 'Proxy-Authorization'])}
            body = json.dumps(result).encode()
            self.send_response(200)
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        if self.path == '/redirect':
            self.send_response(302)
            self.send_header('Location', '/asset')
            self.end_headers()
            return
        body = ('treer-e2e-' + name + ':' + self.path).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'text/plain')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *_): pass

class Echo(socketserver.BaseRequestHandler):
    def handle(self):
        while True:
            data = self.request.recv(65536)
            if not data: break
            self.request.sendall(data)
        self.request.shutdown(socket.SHUT_WR)

class EchoServer(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
https = http.server.ThreadingHTTPServer(('127.0.0.1', port + 2), Http)
http = http.server.ThreadingHTTPServer(('127.0.0.1', port), Http)
tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
tls.load_cert_chain('/tmp/treer-e2e-cert.pem', '/tmp/treer-e2e-key.pem')
https.socket = tls.wrap_socket(https.socket, server_side=True)
echo = EchoServer(('127.0.0.1', port + 1), Echo)
threading.Thread(target=http.serve_forever, daemon=True).start()
threading.Thread(target=https.serve_forever, daemon=True).start()
threading.Thread(target=echo.serve_forever, daemon=True).start()
identity = subprocess.run(['treer', 'whoami'], capture_output=True, text=True)
print('IDENTITY ' + identity.stdout.strip(), flush=True)
startup = subprocess.run(['treer', 'agent', 'startup', 'set', '--cwd', '.', '--', 'python3', '/fixtures/worker.py', name, str(port)], capture_output=True, text=True)
print('STARTUP ' + startup.stdout.strip() + startup.stderr.strip(), flush=True)
print('READY ' + name, flush=True)
children = []
for line in sys.stdin:
    try:
        req = json.loads(line)
        argv = req.get('program') or ['treer'] + req['argv']
        if req.get('background'):
            child = subprocess.Popen(argv, stdin=subprocess.DEVNULL)
            children.append(child)
            result = {'pid': child.pid, 'returncode': 0}
        else:
            result = subprocess.run(argv, input=req.get('stdin', ''), capture_output=True, text=True, timeout=45)
            result = {'returncode': result.returncode, 'stdout': result.stdout, 'stderr': result.stderr}
        print('RESULT ' + json.dumps({'label': req['label'], **result}), flush=True)
    except Exception as e:
        print('RESULT ' + json.dumps({'label': req.get('label'), 'error': str(e)}), flush=True)
