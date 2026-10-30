import numpy as np
import pytest
from rawtl.color import srgb_oetf, pad_even_rgb8, SRGBTransfer
from rawtl.types import LinearFrame

def test_oetf_reference_values():
    src=np.array([0.0,0.0031308,0.18,1.0],dtype=np.float32)
    out=srgb_oetf(src)
    assert out.tolist()==pytest.approx([0.,0.040449936,0.46135613,1.],abs=2e-6)

def test_even_pad_replicates_border():
    img=np.arange(3*5*3,dtype=np.uint8).reshape((3,5,3))
    dst=pad_even_rgb8(img)
    assert dst.shape==(4,6,3)
    np.testing.assert_array_equal(dst[3,:,:],dst[2,:,:])
    np.testing.assert_array_equal(dst[:,5,:],dst[:,4,:])

def test_rgb8_linear_input():
    linear=LinearFrame(np.full((2,2,3),0.18,dtype=np.float32))
    out=SRGBTransfer().to_rgb8(linear)
    assert np.all(out==118)
    with pytest.raises(Exception):
        srgb_oetf(np.array([float("nan")]))
