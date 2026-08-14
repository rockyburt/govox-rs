#!/usr/bin/env python3
"""Dump govox-py's fully-resolved default configuration as JSON.

Reference generator for the M1 config parity test. Run against the *pinned*
source, never the live checkout:

    ./tools/parity-gen/pinned-source.sh
    PYTHONPATH=tools/parity-gen/.parity-src/src \\
      <interpreter> tools/parity-gen/config_dump.py > corpus/config-defaults.json

The environment is scrubbed so the result is the shipped defaults alone: no user
config file, no GOVOX__ overrides. That makes the output a property of the pinned
commit rather than of whoever ran it.
"""

from __future__ import annotations

import json
import os
import sys
import tempfile


def main() -> int:
    # Point XDG_CONFIG_HOME at an empty directory so no user config is layered
    # in, and drop every GOVOX__ override the caller happened to have set.
    scratch = tempfile.mkdtemp(prefix="govox-config-dump-")
    os.environ["XDG_CONFIG_HOME"] = scratch
    for key in [k for k in os.environ if k.startswith("GOVOX__")]:
        del os.environ[key]

    from govox.config import Config

    config = Config.load()

    # mode="json" renders StrEnum members as their string values, matching how
    # serde serialises the Rust enums.
    payload = config.model_dump(mode="json")
    json.dump(payload, sys.stdout, indent=2, sort_keys=True)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
