from datetime import datetime
from pathlib import Path

import pytest

from playbox_judge.artifacts import ENV, resolve_run_dir, volume_mounted

NOW = datetime(2026, 11, 3, 9, 30, 15)


def test_explicit_wins_then_the_environment_then_nothing(tmp_path):
    root = tmp_path / "store"
    root.mkdir()
    assert resolve_run_dir(str(tmp_path / "mine"), "e4", NOW, {ENV: str(root)}) == tmp_path / "mine"
    assert resolve_run_dir(None, "e4", NOW, {ENV: str(root)}) == root / "runs" / "e4-20261103-093015"
    assert resolve_run_dir(None, "e4", NOW, {}) is None and resolve_run_dir(None, "e4", NOW, {ENV: ""}) is None


def test_nothing_is_created_and_an_existing_run_is_never_reused(tmp_path):
    root = tmp_path / "store"
    root.mkdir()
    run = resolve_run_dir(None, "e4", NOW, {ENV: str(root)})
    assert not run.exists() and not (root / "runs").exists()
    run.mkdir(parents=True)
    with pytest.raises(ValueError, match="already exists"):
        resolve_run_dir(None, "e4", NOW, {ENV: str(root)})


def test_a_missing_root_is_an_error_not_a_new_directory(tmp_path):
    with pytest.raises(ValueError, match="volume mounted"):
        resolve_run_dir(None, "e4", NOW, {ENV: str(tmp_path / "gone")})


def test_a_path_on_an_unmounted_volume_is_refused(tmp_path):
    ghost = Path("/Volumes/definitely-not-a-mounted-drive-xyz")
    assert not ghost.exists(), "the test needs a name that is not mounted"
    assert volume_mounted(ghost / "run") is False
    assert volume_mounted(tmp_path / "elsewhere") is True and volume_mounted(Path("/Volumes")) is True
    with pytest.raises(ValueError, match="not mounted"):
        resolve_run_dir(str(ghost / "run"), "e4", NOW, {})
    assert not ghost.exists(), "refusing must not create the path"


def test_a_mounted_volume_is_accepted():
    if not Path("/Volumes").is_dir():
        pytest.skip("/Volumes exists on macOS only")
    mounted = [p for p in Path("/Volumes").iterdir() if p.is_dir()]
    if not mounted:
        pytest.skip("no volume mounted")
    assert volume_mounted(mounted[0] / "anything" / "below") is True
