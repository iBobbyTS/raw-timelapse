"""Independent DNG Chapter 6 oracle (Decimal, 45 digits).

No production code, SDK transform, or output matrix is used to compute expected
values. Build AB*CC*CM, invert, adapt selected white to D50 once, then D50 to D60.
Derive AP1-to-XYZ from chromaticities rather than copying the production matrix.
SDK's documented saturation normalization is a separate scalar (1/white.Y).
"""
from decimal import Decimal as D, getcontext
getcontext().prec = 45

def matrix(rows):
    return [[D(str(x)) for x in row] for row in rows]

def mm(a, b):
    return [[sum(x*y for x,y in zip(row,col)) for col in zip(*b)] for row in a]

def mv(a, v):
    return [sum(x*y for x,y in zip(row,v)) for row in a]

def inv(a):
    # Adjugate/cofactor inversion; independent of production Gauss-Jordan.
    def minor(i,j):
        b=[[a[r][c] for c in range(3) if c!=j] for r in range(3) if r!=i]
        return b[0][0]*b[1][1]-b[0][1]*b[1][0]
    cof=[[(-1)**(i+j)*minor(i,j) for j in range(3)] for i in range(3)]
    det=sum(a[0][i]*cof[0][i] for i in range(3))
    return [[cof[j][i]/det for j in range(3)] for i in range(3)]

def xy(x,y):
    x,y=D(str(x)),D(str(y))
    return [x/y,D(1),(1-x-y)/y]

B=matrix([[.8951,.2664,-.1614],[-.7502,1.7135,.0367],[.0389,-.0685,1.0296]])
D50=xy('.3457','.3585')
D60=xy('.32168','.33767')
CM=matrix([[1.843,-1.2288,.1003],[-.5888,1.5271,.3387],[-.0001,.0219,1.0795]])
NEUTRAL=[D('.6050808314'),D(1),D('.4825046041')]
IDENTITY=matrix([[1,0,0],[0,1,0],[0,0,1]])

def cat(w1,w2):
    a,b=mv(B,w1),mv(B,w2)
    diag=[[b[i]/a[i] if i==j else D(0) for j in range(3)] for i in range(3)]
    return mm(inv(B),mm(diag,B))

def reference(neutral=NEUTRAL,ab=IDENTITY,cc=IDENTITY):
    # Adobe profile ingestion normalizes CM at D50 and rounds to 4 decimals.
    # Model this public-source storage step explicitly; it is not a CAT.
    scale=max(mv(CM,D50))
    cm=[[x/scale if not D('.99')<=scale<=D('1.01') else x for x in row] for row in CM]
    cm=[[x.quantize(D('.0001')) for x in row] for row in cm]
    x=mm(ab,mm(cc,cm))
    camera_to_xyz=inv(x)
    n=[v/max(neutral) for v in neutral]
    raw_white=mv(camera_to_xyz,n)
    white=[v/raw_white[1] for v in raw_white]
    camera_to_d50=mm(cat(white,D50),camera_to_xyz)
    camera_to_d50=[[v/raw_white[1] for v in row] for row in camera_to_d50]
    # Derive AP1 matrix using its three primary xy values and D60 white.
    primaries=[xy('.713','.293'),xy('.165','.830'),xy('.128','.044')]
    columns=[list(c) for c in zip(*primaries)]
    factors=mv(inv(columns),D60)
    ap1_to_xyz=[[columns[i][j]*factors[j] for j in range(3)] for i in range(3)]
    final=mm(inv(ap1_to_xyz),mm(cat(D50,D60),camera_to_d50))
    return camera_to_d50,final
