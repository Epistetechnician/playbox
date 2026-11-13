"""The machine's security report, as `playbox secure` writes it, for the System view.

The hub never judges the machine itself. It runs the CLI (`playbox secure --json`), keeps the newest report, and shows it with its age.
A run happens shortly after the hub starts, every few hours after that, and whenever the owner presses Re-check. The CLI's fuzz run starts
a throwaway copy of this same hub on another port with an empty state directory; it never touches this one."""
import json
import os
import shutil
import subprocess
import threading
import time
from pathlib import Path

STALE = 24 * 3600
MIN_GAP = 20          # seconds between runs, so Re-check can't be used to keep the machine busy


def _home():
    return Path(os.environ.get("PLAYBOX_HOME") or Path.home() / ".playbox")


class Machine:
    def __init__(self, port):
        self.port = port
        self.lock = threading.Lock()
        self.running = False
        self.started = 0.0
        self.error = ""
        self.report = self._load()

    def _load(self):
        try:
            r = json.loads((_home() / "security.json").read_text())
            return r if isinstance(r, dict) and isinstance(r.get("checks"), list) else None
        except (OSError, ValueError):
            return None

    def cli(self):
        return os.environ.get("PLAYBOX_CLI") or shutil.which("playbox")

    def view(self):
        r = self.report
        return {"report": r, "running": self.running, "error": self.error,
                "age": int(time.time() - r["at"]) if r and r.get("at") else None, "stale": bool(r and time.time() - r.get("at", 0) > STALE)}

    def start(self, fix=False):
        """Begin a run in the background. False when one is running or one just finished. `fix` also repairs file permissions, nothing else."""
        with self.lock:
            if self.running or time.time() - self.started < MIN_GAP:
                return False
            if not self.cli():
                self.error = "the `playbox` command isn't on this hub's PATH"
                return False
            self.running, self.started, self.error = True, time.time(), ""
        threading.Thread(target=self._run, args=(fix,), daemon=True).start()
        return True

    def _run(self, fix=False):
        try:
            p = subprocess.run([self.cli(), "secure", "--json", "--port", str(self.port)] + (["--fix"] if fix else []), capture_output=True, text=True, timeout=240, stdin=subprocess.DEVNULL)
            line = next((l for l in reversed(p.stdout.splitlines()) if l.startswith("{")), "")
            r = json.loads(line)
            if isinstance(r, dict) and isinstance(r.get("checks"), list):
                self.report = r
            else:
                self.error = "the check returned something unreadable"
        except subprocess.TimeoutExpired:
            self.error = "the check took longer than 4 minutes and was stopped"
        except (OSError, ValueError):
            self.error = "couldn't run `playbox secure`; update the playbox command"
        finally:
            self.running = False

    def schedule(self, first=45, every=6 * 3600):
        """Run once soon after the hub starts, then every `every` seconds."""
        def loop():
            time.sleep(first)
            while True:
                self.start()
                time.sleep(every)
        threading.Thread(target=loop, daemon=True).start()
