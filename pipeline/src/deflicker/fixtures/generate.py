"""One-time fixture generation from frozen f6d3ae7 legacy scenario inputs.
Not part of Rust runtime or test execution. Run from repository root with NumPy.
Stores little-endian f64 rows then the independently injected flicker truth.
No oracle outputs are stored; Rust tests retain the original assertions.
"""
import ast
import shutil
from pathlib import Path
import numpy as np

ROOT = Path(__file__).resolve().parents[4]
LEGACY = ROOT / "git-worktree/main-legacy"
OUT = Path(__file__).resolve().parent
class Measurement:
    def __init__(self, logs, *args):
        self.log_luminance = logs
source = ast.parse((LEGACY / "tests/test_exposure_core.py").read_text())
function = next(n for n in source.body if isinstance(n, ast.FunctionDef) and n.name == "synthetic")
ns = {"np": np, "FrameMeasurement": Measurement}
exec(compile(ast.Module(body=[function], type_ignores=[]), "legacy synthetic", "exec"), ns)
for n, noise, polluted in [(240, 0., 0), (300, .015, 24), (900, .015, 24), (100, 0., 0)]:
    ms, truth = ns['synthetic'](n, noise=noise, polluted=polluted, flash=n != 100)
    np.concatenate([np.array([m.log_luminance for m in ms]).ravel(), truth]).astype('<f8').tofile(OUT / f'core-{n}.bin')
lines = ["// NumPy PCG64 default_rng normal fixtures from the frozen legacy seeds."]
for seed, sd in [(20261030, .05), (20261031, .05), (5, .05), (7, .05), (2, .05), (11, .05)]:
    values = np.random.default_rng(seed).normal(0., sd, 96)
    lines.append(f'pub const OFF_{seed}: [f64;96] = [' + ','.join(repr(v) for v in values.tolist()) + '];')
noise = np.random.default_rng(3).normal(0., .004, 28)
lines.append('pub const GRAPH_NOISE: [f64;28] = [' + ','.join(repr(v) for v in noise.tolist()) + '];')
(OUT / 'random.rs').write_text('\n'.join(lines)+'\n')
for name in ['frame-first', 'frame-mid', 'frame-first-ev-plus-0.5']:
    shutil.copyfile(ROOT / f'.agent-work/handoffs/deflicker-node/samples/{name}.analysis.json', OUT / f'{name}.analysis.json')
