import numpy as np
import pytest
from rawtl.types import LinearFrame
from rawtl.exposure.measurement import trimmed_linear_mean,measure_frame

def image(value=.18):
    return LinearFrame(np.full((320,480,3),value,dtype=np.float32),None,"unavailable",0.0,False)

def test_gain_equivariance():
    a=image(.18)
    b=image(.36)
    ma=measure_frame(a);mb=measure_frame(b)
    np.testing.assert_allclose(mb.log_luminance-ma.log_luminance,1,atol=1e-6)
    assert ma.valid.all()

def test_negative_noise_preserved_before_mean():
    a=np.concatenate([np.full(100,-.01),np.full(900,.11)])
    assert trimmed_linear_mean(a)>0
    black=measure_frame(image(0.))
    assert not black.valid.any()

def test_saturation_rejects_affected_tiles():
    frame=image(.2)
    frame.saturation_mask=np.ones((320,480),dtype=bool)
    m=measure_frame(frame)
    assert not m.valid.any()
    assert np.all(m.saturation_fraction==1.)

def test_bad_roi_and_hot_pixels():
    a=image(.2)
    a.rgb[0,0,:]=1.
    assert np.all(measure_frame(a).valid)
    with pytest.raises(Exception,match="ROI_TOO_SMALL"):
        measure_frame(a,roi=(0,0,.01,.01))
