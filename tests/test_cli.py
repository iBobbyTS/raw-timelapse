from fractions import Fraction
import pytest
from rawtl.cli import build_parser, load_steps, parse_roi, positive_fps
from rawtl.types import CancellationToken, ProcessingCancelled

def test_help_and_fraction():
    p=build_parser()
    assert p.parse_args(["run","images","-o","out","--fps","30000/1001"]).fps == Fraction(30000,1001)
    assert p.parse_args(["probe","images"]).action=="probe"
    assert positive_fps("24") == Fraction(24)
    with pytest.raises(Exception):
        positive_fps("0")

def test_roi():
    assert parse_roi("0,0,1,1")== (0.,0.,1.,1.)
    with pytest.raises(Exception):
        parse_roi("0.8,0.8,0.5,0.5")

def test_cancel():
    t=CancellationToken()
    t.cancel()
    with pytest.raises(ProcessingCancelled):
        t.check()
