"""O(N) memory 5-diagonal robust Whittaker smoothing using SciPy LAPACK."""
from __future__ import annotations
from dataclasses import dataclass
import math
import numpy as np
from scipy.linalg import solve_banded

@dataclass
class TrendFit:
    trend: np.ndarray
    converged: bool
    iterations: int

def lambda_for_period(period: int) -> float:
    if type(period) is not int or period<6:
        raise ValueError("period must be integer >=6")
    return 1/(16*math.sin(math.pi/period)**4)

def difference_norm_diagonals(n: int) -> tuple[np.ndarray,np.ndarray,np.ndarray]:
    """D2.T @ D2 diagonals assembled without NxN matrices."""
    if n<3:
        raise ValueError("second differences require N>=3")
    main=np.zeros(n,dtype=np.float64)
    first=np.zeros(n-1,dtype=np.float64)
    second=np.ones(n-2,dtype=np.float64)
    main[:-2]+=1
    main[1:-1]+=4
    main[2:]+=1
    first[:-1]-=2
    first[1:]-=2
    return main,first,second

def solve_weighted(values: np.ndarray,weights: np.ndarray,lam: float) -> np.ndarray:
    n=len(values)
    d0,d1,d2=difference_norm_diagonals(n)
    w=np.asarray(weights,dtype=np.float64)
    y=np.where(np.isfinite(values),values,0.).astype(np.float64)
    ab=np.zeros((5,n),dtype=np.float64)
    ab[2,:]=w+lam*d0
    ab[1,1:]=lam*d1;ab[0,2:]=lam*d2
    ab[3,:-1]=lam*d1;ab[4,:-2]=lam*d2
    # Missing observations must be zeroed BEFORE forming RHS: 0 * nan == nan.
    return solve_banded((2,2),ab,w*y,overwrite_ab=True,check_finite=False)

def huber_whittaker(values: np.ndarray,valid: np.ndarray,period: int=60,
    *,delta: float=.1,max_iter: int=100,tol: float=1e-5) -> TrendFit:
    y=np.asarray(values,dtype=np.float64)
    q=np.asarray(valid,dtype=bool)
    if y.ndim!=1 or y.shape!=q.shape or np.count_nonzero(q)<8:
        return TrendFit(np.full_like(y,np.nan),False,0)
    if not np.isfinite(y[q]).all():
        return TrendFit(np.full_like(y,np.nan),False,0)
    lam=lambda_for_period(min(period,len(y)))
    weights=q.astype(np.float64)
    z=solve_weighted(y,weights,lam)
    for iteration in range(1,max_iter+1):
        residual=np.where(q,y-z,0.)
        weights=q*np.minimum(1.,delta/np.maximum(np.abs(residual),1e-12))
        new=solve_weighted(y,weights,lam)
        change=float(np.max(np.abs(new-z)))
        z=new
        if change<=tol:
            return TrendFit(z,True,iteration)
    return TrendFit(z,False,max_iter)
