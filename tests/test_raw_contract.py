import numpy as np
import pytest
from rawtl.raw_backend import RawpyRenderer
from rawtl.types import RawRenderOptions, RawDecodeError

class MockSizes:
    width,height,flip=10,8,0
class MockMetadata:
    make,model="Canon","EOS Test"
class MockRaw:
    sizes=MockSizes()
    metadata=MockMetadata()
    camera_whitebalance=(2.,1.,1.7,1.)
    daylight_whitebalance=(2.1,1.,1.6,1.)
    def __enter__(self): return self
    def __exit__(self,*args): return None
    def postprocess(self,**kwargs):
        self.backend.params=kwargs
        return np.full((8,10,3),32768,dtype=np.uint16)
class MockLib:
    __version__="fake"
    libraw_version="fake"
    class ColorSpace:
        sRGB="srgb"
    class HighlightMode:
        Clip="clip"
    def imread(self,path):
        r=MockRaw()
        r.backend=self
        return r

def test_fixed_wb_and_gain_mapping():
    fake=MockLib()
    r=RawpyRenderer(fake)
    metadata=r.inspect("test.cr3")
    assert metadata.camera=="Canon EOS Test"
    wb=r.fixed_wb(metadata)
    f=r.render_linear("test.cr3",1.,RawRenderOptions(wb))
    assert f.rgb.shape==(8,10,3)
    assert fake.params["exp_shift"]==2.
    assert fake.params["gamma"]==(1,1)
    assert fake.params["no_auto_bright"] is True
    assert fake.params["adjust_maximum_thr"]==0.
    assert fake.params["use_auto_wb"] is False
    assert fake.params["use_camera_wb"] is False
    assert fake.params["output_bps"]==16
    assert f.saturation_mask_quality=="approximate"
    assert np.all(f.rgb<1)

def test_invalid_ev_does_not_render():
    r=RawpyRenderer(MockLib())
    with pytest.raises(RawDecodeError):
        r.render_linear("a.cr3",3.,RawRenderOptions((2.,1.,1.,1.)))
