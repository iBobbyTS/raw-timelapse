#include "highlight.h"
#include "frame.h"
#include <algorithm>
#include <cmath>
#include <iostream>
#include <limits>
#include <stdexcept>
using namespace frame::highlight;
void require(bool ok, const char *message) { if (!ok) throw std::runtime_error(message); }
Evidence evidence() { return {true, {1,1,1}, {.005,.005,.005}, "controlled integer ceiling"}; }
std::vector<float> image(int w,int h,std::array<double,3> p) {
  std::vector<float> a(w*h*3);for(size_t i=0;i<a.size();++i)a[i]=float(p[i%3]);return a;
}
int main() { try {
  const std::array<double,3> n{.5,1,.7}; auto e=evidence();
  auto untouched=image(32,32,{-.01,.4,.8});
  require(reconstruct(untouched,32,32,n,e).camera.empty(),"unsaturated signed pixels changed");
  auto hdr=image(32,32,{2,4,3});e.supported=false;
  require(reconstruct(hdr,32,32,n,e).camera.empty(),"unknown HDR endpoints were guessed");e=evidence();
  // Uniform neutral highlight: one missing channel; exact original R/B anchors.
  auto partial=image(64,64,{.75,1,1.05}); // R=0.75 reliable; G/B clipped observations below.
  for(size_t i=0;i<partial.size();i+=3) partial[i+2]=1;
  auto original=partial;auto p=reconstruct(partial,64,64,n,e);
  require(partial==original,"reconstruction overwrote observations");
  require(p.camera[0]==partial[0],"reliable red changed");
  require(std::abs(p.camera[1]-1.5)<1e-6,"partial neutral green not recovered");
  require(std::abs(p.camera[2]-1.05)<1e-6,"dual-channel neutral blue not recovered");
  // Single-channel truncation with known two-channel truth.
  auto single=image(64,64,{.65,1,.91});auto s=reconstruct(single,64,64,n,e);
  require(s.camera[0]==single[0] && s.camera[2]==single[2],"reliable single-channel anchors changed");
  require(std::abs(s.camera[1]-1.3)<1e-6,"single-channel neutral truth missed");
  // Colored material: nearby unsaturated colors provide the missing ratio.
  const std::array<double,3> color{.8,1,.56};
  auto colored=image(64,64,{.4,.5,.28});
  for(int y=24;y<40;++y)for(int x=24;x<40;++x){size_t i=(y*64+x)*3;colored[i]=.96;colored[i+1]=1;colored[i+2]=.672;}
  auto c=reconstruct(colored,64,64,n,e);size_t center=(32*64+32)*3;
  require(c.camera[center]==colored[center] && c.camera[center+2]==colored[center+2],"colored anchors changed");
  require(std::abs(c.camera[center+1]-1.2)<.04,"colored highlight ratio not recovered");
  require(c.camera[center+1] < 1.5,"colored highlight bleached by neutral fallback");
  // Full saturation has no texture: finite minimum neutral extension, not white clamp.
  auto full=image(32,32,{1,1,1});auto f=reconstruct(full,32,32,n,e);
  require(f.statistics.full==1024,"full saturation classification wrong");
  for(size_t i=0;i<f.camera.size();i+=3){require(f.camera[i]==1 && f.camera[i+1]==2,"full neutral scale wrong");require(std::abs(f.camera[i+2]-1.4)<1e-6,"full neutral blue wrong");}
  require(f.camera==reconstruct(full,32,32,n,e).camera,"not deterministic");
  // Smooth approach to a ceiling: no switch at the final usable-channel loss.
  float previous=0;
  for(int k=0;k<=100;++k){double v=.994+double(k)*.00007;auto a=image(16,16,{.5*v,std::min(1.,v),.7*v});auto z=reconstruct(a,16,16,n,e);float value=z.camera.empty()?a[1]:z.camera[1];if(k)require(std::abs(value-previous)<.001,"ceiling transition discontinuity");previous=value;}
  // Continuous seed withdrawal: noise near the reliable cutoff cannot switch
  // an entire cell's chroma prior off in one step.
  previous=0;
  for(int k=0;k<=100;++k){
    auto a=image(64,64,{.4,.99+double(k)*.00006,.28});
    size_t i=(32*64+32)*3;a[i]=.65;a[i+1]=1;a[i+2]=.455;
    auto z=reconstruct(a,64,64,n,e);float value=z.camera[i+1];
    if(k && std::abs(value-previous)>=.025) std::cerr << "seed k="<<k<<" previous="<<previous<<" value="<<value<<" delta="<<value-previous<<"\n";
    if(k)require(std::abs(value-previous)<.025,"seed cutoff discontinuity");previous=value;
  }
  bool cancelled=false;
  try { reconstruct(full,32,32,n,e,[](){throw std::runtime_error("cancel");}); }
  catch(const std::runtime_error&){cancelled=true;}
  require(cancelled,"cancellation callback not invoked");
  bool failed=false;try{auto invalid=image(1,1,{1,1,1});invalid[0]=std::numeric_limits<float>::quiet_NaN();reconstruct(invalid,1,1,n,e);}catch(const frame::Error&){failed=true;}require(failed,"nonfinite observation accepted");
  std::cout<<"PASS highlight identity, signed/HDR preservation, single/dual truth, spatial color, full neutral, continuous ceiling, deterministic, invalid boundary\n";
  return 0;
} catch(const std::exception &e){std::cerr<<e.what()<<'\n';return 1;} }
