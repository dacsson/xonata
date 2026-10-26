#!/usr/bin/env python3
"""Serve Xonata and forward /llm/ requests to a fixed LiteLLM origin for local testing."""

import argparse
from functools import partial
from http.client import HTTPConnection, HTTPSConnection, HTTPException
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
import os
from pathlib import Path
from urllib.parse import urlsplit


class ProxyHandler(SimpleHTTPRequestHandler):
    def __init__(self, *args, upstream, **kwargs):
        self.upstream = upstream
        super().__init__(*args, **kwargs)

    def do_GET(self):
        if self.path.startswith('/llm/'):
            self.proxy()
        else:
            super().do_GET()

    def do_POST(self):
        if self.path.startswith('/llm/'):
            self.proxy()
        else:
            self.send_error(404)

    def proxy(self):
        if self.headers.get('Transfer-Encoding'):
            self.send_error(400, 'Use Content-Length for request bodies')
            return
        try:
            length = int(self.headers.get('Content-Length', '0'))
        except ValueError:
            self.send_error(400, 'Invalid Content-Length')
            return
        if not 0 <= length <= 4 * 1024 * 1024:
            self.send_error(413, 'Request body exceeds 4 MiB')
            return

        headers = {name: self.headers[name] for name in
                   ('Authorization', 'Content-Type', 'Accept') if name in self.headers}
        connection_type = HTTPSConnection if self.upstream.scheme == 'https' else HTTPConnection
        connection = connection_type(self.upstream.hostname, self.upstream.port, timeout=130)
        try:
            connection.request(self.command, self.path[len('/llm'):],
                               body=self.rfile.read(length), headers=headers)
            response = connection.getresponse()
        except (OSError, HTTPException):
            connection.close()
            self.send_error(502, 'Could not reach the configured LiteLLM server')
            return

        try:
            self.send_response(response.status)
            for name in ('Content-Type', 'Content-Length', 'Content-Encoding'):
                if response.getheader(name) is not None:
                    self.send_header(name, response.getheader(name))
            self.end_headers()
            while chunk := response.read1(64 * 1024):
                self.wfile.write(chunk)
        except (OSError, HTTPException):
            # Closing either connection ends the response, including cancelled browser requests.
            pass
        finally:
            connection.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--upstream', default=os.environ.get(
        'XONATA_LITELLM_UPSTREAM', 'http://localhost:4000'))
    parser.add_argument('--port', type=int, default=8000)
    parser.add_argument('--bind', default='127.0.0.1', help='IPv4 bind address (default: 127.0.0.1)')
    args = parser.parse_args()
    try:
        upstream = urlsplit(args.upstream)
        if (upstream.scheme not in ('http', 'https') or not upstream.hostname
                or upstream.path not in ('', '/') or upstream.query or upstream.fragment
                or upstream.username is not None or upstream.password is not None):
            raise ValueError('Use an HTTP(S) origin without a path or credentials')
        _ = upstream.port  # Validate an explicitly supplied port before starting the server.
    except ValueError as error:
        parser.error(str(error))
    handler = partial(ProxyHandler, upstream=upstream,
                      directory=str(Path(__file__).resolve().parents[1] / 'web'))
    with ThreadingHTTPServer((args.bind, args.port), handler) as server:
        print(f'Listening on http://{args.bind}:{server.server_port}; stop with Ctrl+C.', flush=True)
        try:
            server.serve_forever()
        except KeyboardInterrupt:
            pass


if __name__ == '__main__':
    main()
