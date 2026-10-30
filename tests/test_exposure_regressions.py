import numpy as np
import pytest
from rawtl.exposure.trends import solve_weighted,lambda_for_period
from rawtl.exposure.measurement import FrameMeasurement
from rawtl.exposure.estimation import estimate_exposure

CENTERS=np.array([((i+.5)/12,(j+.5)/8) for j in range(8) for i in range(12)])

def test_nan_missing_not_rhs_nan():
    x=np.linspace(-2,-1,60)
    x[18:21]=np.nan
    w=np.isfinite(x).astype(float)
    z=solve_weighted(x,w,lambda_for_period(60))
    assert np.isfinite(z).all()

def test_natural_boundary_and_10000_frames():
    x=np.linspace(-2,-1,10000,dtype=float)
    z=solve_weighted(x,np.ones(len(x)),lambda_for_period(60))
    assert abs(z[0]-x[0])<1e-7 and abs(z[-1]-x[-1])<1e-7

def test_nonfinite_explicitly_valid_rejected():
    ms=[FrameMeasurement(np.full(96,-2.),np.ones(96,bool),CENTERS,np.zeros(96),
        320,240,"approximate") for _ in range(20)]
    ms[2].log_luminance[7]=np.nan
    with pytest.raises(Exception,match="nonfinite"):
        estimate_exposure(ms)
