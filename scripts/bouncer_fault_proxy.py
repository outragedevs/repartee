"""Disposable loopback TCP fault proxy; forwards bytes without recording them."""

import http.server
import json
import select
import socket
import socketserver
import threading


class FaultProxy:
    def __init__(self, upstream_port):
        self.upstream_port = upstream_port
        self.lock = threading.Lock()
        self.blocked = False
        self.connections = set()

    def forward(self, pair, source, data):
        destination = pair[1] if source is pair[0] else pair[0]
        destination.sendall(data)
        return True

    def extra_control(self, path):
        return None

    def cut(self):
        with self.lock:
            self.blocked = True
            connections = list(self.connections)
        for pair in connections:
            for stream in pair:
                try:
                    stream.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass
        return len(connections)

    def __enter__(self):
        proxy = self

        class Relay(socketserver.BaseRequestHandler):
            def handle(self):
                with proxy.lock:
                    if proxy.blocked:
                        return
                try:
                    upstream = socket.create_connection(('127.0.0.1', proxy.upstream_port), timeout=5)
                except OSError:
                    return
                pair = (self.request, upstream)
                with upstream:
                    with proxy.lock:
                        if proxy.blocked:
                            return
                        proxy.connections.add(pair)
                    try:
                        while True:
                            ready, _, _ = select.select(pair, [], [], .1)
                            for source in ready:
                                data = source.recv(65536)
                                if not data:
                                    return
                                if not proxy.forward(pair, source, data):
                                    return
                    except OSError:
                        pass
                    finally:
                        with proxy.lock:
                            proxy.connections.discard(pair)

        class Control(http.server.BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_POST(self):
                if self.path == '/cut':
                    result = {'closed': proxy.cut()}
                elif self.path == '/resume':
                    with proxy.lock:
                        proxy.blocked = False
                    result = {'resumed': True}
                else:
                    result = proxy.extra_control(self.path)
                    if result is None:
                        self.send_error(404)
                        return
                body = json.dumps(result).encode()
                self.send_response(200)
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        class TcpServer(socketserver.ThreadingTCPServer):
            daemon_threads = True

        self.tcp = TcpServer(('127.0.0.1', 0), Relay)
        self.control = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Control)
        self.threads = [threading.Thread(target=server.serve_forever, daemon=True)
                        for server in (self.tcp, self.control)]
        for thread in self.threads:
            thread.start()
        self.port = self.tcp.server_address[1]
        self.control_url = f'http://127.0.0.1:{self.control.server_address[1]}'
        return self

    def __exit__(self, *args):
        self.cut()
        for server in (self.tcp, self.control):
            server.shutdown()
            server.server_close()
        for thread in self.threads:
            thread.join(timeout=5)
