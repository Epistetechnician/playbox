"""Where a run writes its artifacts (pure; the experiment scripts call this before creating anything)."""
from __future__ import annotations

from collections.abc import Mapping
from datetime import datetime
from pathlib import Path

ENV = "PLAYBOX_ARTIFACTS"


VOLUMES = Path("/Volumes")


def volume_mounted(path: Path) -> bool:
    """False for a path under `/Volumes/<name>` when `<name>` is not mounted. Creating such a path would silently make a
    directory on the internal disk (macOS leaves `/Volumes/<name>` creatable as a plain folder when the drive is out)."""
    p = path.absolute()
    if p.parts[:2] != ("/", VOLUMES.name):
        return True
    return len(p.parts) < 3 or (VOLUMES / p.parts[2]).exists()


def resolve_run_dir(explicit: str | None, kind: str, now: datetime, env: Mapping[str, str]) -> Path | None:
    """The run directory: `explicit` if given, else `$PLAYBOX_ARTIFACTS/runs/<kind>-<timestamp>` if that variable is
    set, else None (the caller falls back to a local temporary directory). Never creates anything."""
    if explicit:
        run = Path(explicit)
    elif env.get(ENV):
        root = Path(env[ENV])
        if not root.is_dir():
            raise ValueError(f"{ENV}={root} is not an existing directory: is the volume mounted? (it is not created for you)")
        run = root / "runs" / f"{kind}-{now:%Y%m%d-%H%M%S}"
    else:
        return None
    if not volume_mounted(run):
        raise ValueError(f"{run} is on a volume that is not mounted; refusing to create it on the internal disk")
    if run.exists():
        raise ValueError(f"{run} already exists; a run does not overwrite another")
    return run
