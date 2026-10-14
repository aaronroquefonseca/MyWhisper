#!/usr/bin/python3
from http.server import ThreadingHTTPServer, BaseHTTPRequestHandler
from pathlib import Path
import json
import sys
import time
model = sys.argv[sys.argv.index('--model') + 1]
port = int(sys.argv[sys.argv.index('--port') + 1])
class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args): pass
    def do_POST(self):
        self.rfile.read(int(self.headers.get('Content-Length', '0')))
        if self.path == '/inference':
            Path(model + '.request_started').touch()
            time.sleep(3)
        data = json.dumps({'text':'synthetic completed transcript'}).encode()
        self.send_response(200)
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)
ThreadingHTTPServer(('127.0.0.1', port), Handler).serve_forever()
