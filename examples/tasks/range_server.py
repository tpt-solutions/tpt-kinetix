#!/usr/bin/env python3
"""Serves one directory over HTTP with Range support, for the remote-probe example.

Usage: range_server.py DIRECTORY PORT
Only needed because `tpt-kinetix probe http(s)://...` reads with range requests;
Python's stock http.server ignores Range headers.
"""
import http.server
import os
import re
import sys

RANGE = re.compile(r"bytes=(\d*)-(\d*)$")


class RangeHandler(http.server.SimpleHTTPRequestHandler):
    def send_head(self):
        path = self.translate_path(self.path)
        header = self.headers.get("Range")
        if os.path.isdir(path) or header is None:
            return super().send_head()
        match = RANGE.match(header.strip())
        if not match:
            self.send_error(416)
            return None
        size = os.path.getsize(path)
        first, last = match.group(1), match.group(2)
        if first == "":  # suffix range: the last N bytes
            start, end = max(size - int(last), 0), size - 1
        else:
            start = int(first)
            end = min(int(last), size - 1) if last else size - 1
        if start > end or start >= size:
            self.send_error(416)
            return None
        f = open(path, "rb")
        f.seek(start)
        self.send_response(206)
        self.send_header("Content-Type", self.guess_type(path))
        self.send_header("Accept-Ranges", "bytes")
        self.send_header("Content-Range", f"bytes {start}-{end}/{size}")
        self.send_header("Content-Length", str(end - start + 1))
        self.end_headers()
        self._range_end = end - start + 1
        return f

    def copyfile(self, source, outputfile):
        remaining = getattr(self, "_range_end", None)
        if remaining is None:
            return super().copyfile(source, outputfile)
        while remaining > 0:
            chunk = source.read(min(65536, remaining))
            if not chunk:
                break
            outputfile.write(chunk)
            remaining -= len(chunk)
        self._range_end = None

    def log_message(self, *args):
        pass  # keep the example output readable


if __name__ == "__main__":
    os.chdir(sys.argv[1])
    server = http.server.ThreadingHTTPServer(("127.0.0.1", int(sys.argv[2])), RangeHandler)
    server.serve_forever()
