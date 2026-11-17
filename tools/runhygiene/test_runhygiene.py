import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import runhygiene as rh  # noqa: E402


def kinds(src):
    return [f.kind for f in rh.lint_source(src) if f.probability >= rh.THRESHOLD]


def test_clean_seeded_generator():
    assert kinds("import numpy as np\nrng = np.random.default_rng(0)\nx = rng.normal()\n") == []


def test_unseeded_generator():
    assert kinds("import numpy as np\nrng = np.random.default_rng()\n") == ["unseeded_rng"]


def test_global_numpy_state():
    assert kinds("import numpy as np\nx = np.random.randn(3)\n") == ["global_rng"]


def test_global_state_seeded_once_is_not_reported():
    assert kinds("import numpy as np\nnp.random.seed(0)\nx = np.random.randn(3)\n") == []


def test_torch_without_manual_seed():
    assert kinds("import torch\nx = torch.randn(2)\n") == ["global_rng"]
    assert kinds("import torch\ntorch.manual_seed(1)\nx = torch.randn(2)\n") == []


def test_python_random():
    assert kinds("import random\nx = random.random()\n") == ["global_rng"]
    assert kinds("import random\nr = random.Random(3)\nx = r.random()\n") == []


def test_swallowed_error_only_when_broad():
    assert kinds("try:\n    f()\nexcept Exception:\n    pass\n") == ["swallowed_error"]
    assert kinds("try:\n    f()\nexcept:\n    pass\n") == ["swallowed_error"]
    assert kinds("try:\n    f()\nexcept ValueError:\n    pass\n") == []
    assert kinds("try:\n    f()\nexcept Exception as e:\n    log(e)\n") == []


def test_syntax_error_reported():
    assert kinds("def (:\n") == ["syntax"]


def test_cli_check_exit_code(tmp_path):
    bad = tmp_path / "bad.py"
    bad.write_text("import random\nx = random.random()\n")
    good = tmp_path / "good.py"
    good.write_text("import random\nx = random.Random(1).random()\n")
    assert rh.main([str(bad), "--check"]) == 1
    assert rh.main([str(good), "--check"]) == 0
    assert rh.main([str(bad)]) == 0


def test_frozen_edit_detected(tmp_path):
    def git(*a):
        subprocess.run(["git", "-C", str(tmp_path), "-c", "user.email=t@t", "-c", "user.name=t", "-c", "core.hooksPath=/dev/null", *a], check=True, capture_output=True)

    (tmp_path / "pre.py").write_text("x = 1\n")
    (tmp_path / "frozen.txt").write_text("# frozen\npre*.py\n")
    git("init", "-q")
    git("add", ".")
    git("commit", "-qm", "init")
    assert rh.main([str(tmp_path / "pre.py"), "--frozen", str(tmp_path / "frozen.txt"), "--check"]) == 0
    (tmp_path / "pre.py").write_text("x = 2\n")
    assert rh.main([str(tmp_path / "pre.py"), "--frozen", str(tmp_path / "frozen.txt"), "--check"]) == 1


def test_generator_kwarg_counts_as_seeded():
    assert kinds("import torch\nx = torch.randn(2, generator=g)\n") == []


def test_tests_are_downgraded():
    src = "import torch\nx = torch.randn(2)\n"
    assert [f for f in rh.lint_source(src, "tests/test_a.py") if f.probability >= rh.THRESHOLD] == []


def test_exclude_glob(tmp_path):
    vend = tmp_path / "vend"
    vend.mkdir()
    (vend / "a.py").write_text("import random\nrandom.random()\n")
    assert rh.main([str(tmp_path), "--check"]) == 1
    assert rh.main([str(tmp_path), "--check", "--exclude", "*/vend/*"]) == 0


def test_best_effort_cleanup_is_not_reported():
    assert kinds("try:\n    proc.kill()\nexcept Exception:\n    pass\n") == []
    assert kinds("try:\n    run_trial()\nexcept Exception:\n    pass\n") == ["swallowed_error"]
