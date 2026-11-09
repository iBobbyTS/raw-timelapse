"""Sparse relative-exposure graph: robust pairwise differences -> brightness.

Frames are nodes; reliable matched-background differences between frames are
directed edges (b_j - b_i ~= d_ij). One gauge frame per connected component;
Huber IRLS on a sparse incidence matrix solves the rest. The solver never
constructs a dense N x N matrix and never invents values for disconnected
components.
"""
from __future__ import annotations
from dataclasses import dataclass, field
import math
import numpy as np
from scipy.sparse import csr_matrix, diags
from scipy.sparse.linalg import spsolve, MatrixRankWarning
import warnings

HUBER_DELTA_EV = .02
IRLS_ITERATIONS = 8


@dataclass
class GraphEdge:
    i: int
    j: int
    delta_ev: float            # robust common log2 difference over matched tiles
    weight: float
    support: int               # matched usable tiles
    consensus_fraction: float
    spatial_mad_ev: float
    quadrants: int
    lag: int
    rejects: dict[str, int] = field(default_factory=dict)

    @property
    def reliable(self) -> bool:
        return (self.support >= 24 and self.consensus_fraction >= .6 and
                self.spatial_mad_ev <= .06 and self.quadrants >= 3)


@dataclass
class GraphResult:
    brightness: np.ndarray                 # float64[n], NaN outside components
    component: np.ndarray                  # int[n], -1 = unconstrained
    edge_residuals: np.ndarray             # float64[m]
    edge_p95_abs_residual_ev: float | None
    components: list[list[int]]
    converged: bool
    iterations: int


def _components(n: int, edges: list[GraphEdge]) -> tuple[list[list[int]], np.ndarray]:
    adjacency: list[set[int]] = [set() for _ in range(n)]
    for e in edges:
        adjacency[e.i].add(e.j)
        adjacency[e.j].add(e.i)
    labels = np.full(n, -1, dtype=int)
    components: list[list[int]] = []
    for start in range(n):
        if labels[start] != -1:
            continue
        comp_id = len(components)
        stack, members = [start], []
        labels[start] = comp_id
        while stack:
            node = stack.pop()
            members.append(node)
            for neighbour in adjacency[node]:
                if labels[neighbour] == -1:
                    labels[neighbour] = comp_id
                    stack.append(neighbour)
        components.append(sorted(members))
    return components, labels


def solve_exposure_graph(n: int, edges: list[GraphEdge],
                         *, delta_ev: float = HUBER_DELTA_EV,
                         max_iterations: int = IRLS_ITERATIONS) -> GraphResult:
    """Robust sparse least squares for relative brightness with one gauge per
    connected component."""
    brightness = np.full(n, np.nan, dtype=np.float64)
    if not edges:
        return GraphResult(brightness, np.full(n, -1, dtype=int),
                           np.zeros(0), None, [], True, 0)
    components, labels = _components(n, edges)
    m = len(edges)
    rows = np.repeat(np.arange(m), 2)
    cols = np.array([[e.i, e.j] for e in edges]).ravel()
    vals = np.tile([-1., 1.], m)
    incidence = csr_matrix((vals, (rows, cols)), shape=(m, n))
    d = np.array([e.delta_ev for e in edges], dtype=np.float64)
    raw_weights = np.array([e.weight for e in edges], dtype=np.float64)
    weights = raw_weights / max(float(np.median(raw_weights)), 1e-12)
    converged = True
    iterations = 0
    for comp_id, comp in enumerate(components):
        gauge = comp[0]
        free = np.array([node for node in comp if node != gauge], dtype=int)
        mask = labels[np.array([e.i for e in edges])] == comp_id
        if int(mask.sum()) == 0:
            # A lone node has no photometric evidence: leave it unconstrained
            # (NaN) instead of inventing a gauge value.
            continue
        sub = incidence[mask][:, free]
        target = d[mask]
        comp_weights = weights[mask]
        robust = np.ones(int(mask.sum()), dtype=np.float64)
        previous = None
        if free.size == 0:
            brightness[gauge] = 0.
            continue
        for iteration in range(1, max_iterations + 1):
            iterations = max(iterations, iteration)
            wd = diags(comp_weights * robust)
            normal = sub.T @ wd @ sub
            with warnings.catch_warnings():
                warnings.simplefilter("ignore", MatrixRankWarning)
                try:
                    solution = spsolve(normal, sub.T @ (comp_weights * robust * target))
                except Exception:
                    solution = None
            if solution is None or not np.all(np.isfinite(solution)):
                converged = False
                brightness[gauge] = 0.
                brightness[free] = 0. if solution is None else solution
                break
            brightness[gauge] = 0.
            brightness[free] = solution
            if (previous is not None and previous.size == solution.size and
                    float(np.max(np.abs(solution - previous))) < 1e-10):
                break
            previous = solution
            comp_residuals = np.abs(incidence[mask] @ brightness - target)
            robust = np.minimum(1., delta_ev / np.maximum(comp_residuals, 1e-10))
    residuals = np.asarray(incidence @ brightness - d).ravel()
    finite = residuals[np.isfinite(residuals)]
    p95 = float(np.percentile(np.abs(finite), 95)) if finite.size else None
    return GraphResult(brightness, labels, residuals, p95, components, converged,
                       iterations)
