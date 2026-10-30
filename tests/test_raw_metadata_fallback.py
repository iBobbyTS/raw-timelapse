"""rawpy does not expose a standard make/model property; never invent one."""
import numpy as np
from rawtl.raw_backend import RawpyRenderer
from rawtl.types import RawRenderOptions

class NoMetadataRaw:
    class Sizes:
        width=120
        height=80
        flip=0
    sizes=Sizes()
    camera_whitebalance=[2.,1.,1.5,0.]
    daylight_whitebalance=[2.,1.,1.5,1.]
    raw_pattern=np.array([[0,1],[1,2]])
    color_matrix=np.eye(3,4)
    color_desc=b"RGBG"
    raw_type="flat"
    num_colors=3
    def __enter__(self):return self
    def __exit__(self,*args):pass
class RawMod:
    def imread(self,path):return NoMetadataRaw()

def test_no_undocumented_rawpy_metadata_attribute():
    r=RawpyRenderer(RawMod())
    data=r.inspect("a.CR3")
    assert data.camera=="UNKNOWN_CAMERA"
    assert data.camera_wb==(2.,1.,1.5,1.)
    assert data.sensor_signature
    assert data.raw_type==".cr3"
