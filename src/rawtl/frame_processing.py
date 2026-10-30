"""V1 identity linear-frame processor; reserved for later lens corrections."""
from .types import LinearFrame

class IdentityProcessor:
    def process(self,frame: LinearFrame) -> LinearFrame:
        return frame
