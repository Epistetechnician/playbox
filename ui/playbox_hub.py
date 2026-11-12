#!/usr/bin/env python3
"""Playbox hub: every room you are in, yours and shared, as one installable app.

    python3 ui/playbox_hub.py                 # http://127.0.0.1:7717 ; prints a one-time pairing link
    python3 ui/playbox_hub.py --tailscale     # also publish privately to your tailnet over HTTPS (passkeys, push)
    python3 ui/playbox_hub.py --lan           # phones on your Wi-Fi can pair over plain HTTP
    python3 ui/playbox_hub.py --pair          # mint a link to add another device
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from hub.server import main  # noqa: E402

if __name__ == "__main__":
    main()
