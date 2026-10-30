# Exposure algorithm contract (V1)

The standalone exposure-algorithm plan is the normative source; the implementation MUST not substitute full-frame-average rolling means.

1. Analysis is from fixed-WB, fixed-rendered **linear** RGB. No per-frame exposure auto-scaling. Analyze after linear area downsampling (long edge <= 1024), ROI in display coordinates, 12×8 tiles or reduced grid with >=256 samples per tile.
2. Pixel luminance is 0.2126R+0.7152G+0.0722B. For each tile, use 5%/5% trimmed **linear** mean, then log2. Reject nonfinite values, mean <= max(1e-5,8*quantum), or saturation occupancy > 0.005.
3. Remove only user-confirmed step vector before fitting. Fit each valid tile to Huber-data / second-difference Whittaker smoothing using 64-bit banded IRLS, 100 iterations max, 1e-5 EV threshold, delta=0.1 EV, lambda=1/(16 sin(pi/P)^4), P_eff=min(P,N).
4. Take residual median across tiles; require >=max(8,ceil(K/4)) valid tiles, >=60% within 0.06 EV, scaled MAD<=0.06 EV and consensus tiles from at least 3 of 4 ROI quadrants.
5. Interpolate only internal gaps of <=3 frames between trusted residuals; never span entire corrections or extend edge gaps. Report unreliable values as null / REVIEW_REQUIRED, not silent zero.
6. Raw correction = strength * (-confirmed_step - residual); a single global median reference anchor applies to reliable frames. Preserve separate requested_ev, applied_ev and render_ev, and explicit clipping with status LIMITED.
7. Disabled strength=0 produces zero EV. <8 frames are no-automatic-deflicker but confirmed steps may still apply. Do not auto-derive steps from EXIF.
8. Freeze tile targets from original analysis; verify renderer at -0.25/+0.25 EV on up to five representative RAW frames; track observed error and retry at most twice, never regenerate targets or mix prior encoded frames. Bad probe/validation is reported, not misrepresented as success.
9. No per-frame highlight-protection gain adjustments. Exposure gain is applied to linear sensor/render data before sRGB OETF. 16-bit clipped output and surrogate saturation masks are explicitly approximate.
10. The expected mathematical tests cover constant and linear trends, 25% contaminated tiles, step semantics, gap crossing, saturation, severe low light, numerical nonfinite and memory bounds, and the true-global-flash ambiguity.

The algorithm's outputs are **not** proof of correct natural-scene change classification; users choose the time-scale and optionally an ROI.
