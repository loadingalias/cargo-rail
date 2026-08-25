#!/usr/bin/env python3
"""Serve an installer fixture and report the bound port through a file."""

from __future__ import annotations

import argparse
import functools
import http.server
import os
import pathlib


def main() -> None:
  parser = argparse.ArgumentParser()
  parser.add_argument("--directory", required=True, type=pathlib.Path)
  parser.add_argument("--port-file", required=True, type=pathlib.Path)
  arguments = parser.parse_args()

  handler = functools.partial(
    http.server.SimpleHTTPRequestHandler,
    directory=str(arguments.directory),
  )
  with http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler) as server:
    temporary_port_file = arguments.port_file.with_name(
      f".{arguments.port_file.name}.{os.getpid()}.tmp"
    )
    temporary_port_file.write_text(str(server.server_port), encoding="ascii")
    os.replace(temporary_port_file, arguments.port_file)
    server.serve_forever()


if __name__ == "__main__":
  main()
