#include "lens.h"
#include <cmath>
#include <iostream>
#include <stdexcept>
using namespace frame::lens;
void require(bool v,const char*m){if(!v)throw std::runtime_error(m);}
Calibration calibration(double scale){Calibration c;c.collected=true;c.upstream.fill(Upstream::not_reported);auto&m=c.fallback[1];m.kind=Kind::perspective;m.source="synthetic:analytic-map";m.plane[0].scale=scale;return c;}
int main(){try{
 const unsigned w=257,h=97;constexpr double pi=3.14159265358979323846;Policy quality;quality.sampling=Sampling::quality;Policy bilinear;bilinear.sampling=Sampling::bilinear;
 for(double scale:{.99,1.5})for(double frequency:{.05,.25,.4}){
  auto c=calibration(scale);std::vector<float>pixels(size_t(w)*h*3);
  for(unsigned y=0;y<h;y++)for(unsigned x=0;x<w;x++)for(unsigned ch=0;ch<3;ch++)pixels[(size_t(y)*w+x)*3+ch]=float(std::sin(2*pi*frequency*x));
  auto linear=correct(pixels,w,h,c,true,nullptr,bilinear);auto better=correct(pixels,w,h,c,true,nullptr,quality);double a=0,b=0;
  for(unsigned y=16;y<h-16;y++)for(unsigned x=64;x<w-64;x++){
   const double position=(w-1)*.5+(x-(w-1)*.5)*scale;
   const double truth=frequency*scale>=.5?0:std::sin(2*pi*frequency*position);
   double da=linear.pixels[(size_t(y)*w+x)*3]-truth,db=better.pixels[(size_t(y)*w+x)*3]-truth;a+=da*da;b+=db*db;
  }
  require(b<a,"quality sampling did not improve analytic signal error");
  if(frequency==.25&&scale==.99)require(b<a*.04,"quality passband benefit regressed");
  if(frequency==.4&&scale==1.5)require(b<a*.25,"quality minification anti-aliasing regressed");
 }
 std::vector<float>step(size_t(w)*h*3);for(unsigned y=0;y<h;y++)for(unsigned x=0;x<w;x++)for(unsigned ch=0;ch<3;ch++)step[(size_t(y)*w+x)*3+ch]=x<w/2?-.5f:4.f;
 auto r=correct(step,w,h,calibration(.99),true,nullptr,quality);
 for(float v:r.pixels)require(v>=-.5f&&v<=4.f,"quality step ringing escaped signed HDR footprint extrema");
 Calibration c=calibration(1);c.fallback[1]={};c.fallback[2].kind=Kind::perspective;c.fallback[2].source="synthetic:relative-ca";c.fallback[2].plane[0].scale=.99;c.fallback[2].plane[2].scale=1.01;
 r=correct(step,w,h,c,true,nullptr,quality);for(size_t i=1;i<step.size();i+=3)require(r.pixels[i]==step[i],"relative CA modified green plane");
 require(correct(step,w,h,c,false,nullptr,quality).pixels.empty(),"quality changed off/no-op behavior");
 c.fallback[0].kind=Kind::perspective;c.fallback[0].source="synthetic:vignette";c.fallback[0].plane[0].k[0]=-.4;
 Policy half;half.components={true,false,false};half.vignette_strength=.5;
 r=correct(step,w,h,c,true,nullptr,half);double r2=(128.*128+48.*48)/(257.*257),gain=std::sqrt(1/(1-.4*r2));
 require(std::abs(r.pixels[0]-step[0]*gain)<1e-6,"vignette strength is not exposure-domain interpolation");
 require(r.decisions[1].status=="off"&&r.decisions[2].status=="off","component switches coupled");
 half.vignette_strength=0;require(correct(step,w,h,c,true,nullptr,half).pixels.empty(),"zero strength not exact no-op");
 half.vignette_strength=1.01;bool bad=false;try{correct(step,w,h,c,true,nullptr,half);}catch(...){bad=true;}require(bad,"out-of-range strength accepted");
 // Effective-area certification must cover RGB jointly, and the quality
 // kernel footprint must fit without taking input-edge extension shortcuts.
 c=calibration(1.05);c.fallback[2].kind=Kind::perspective;c.fallback[2].source="synthetic:area-ca";
 c.fallback[2].plane[0].scale=1.02;c.fallback[2].plane[2].scale=1.04;
 const auto bilinear_area=safe_area(c,w,h,true,bilinear),area=safe_area(c,w,h,true,quality);
 require(area[0]>bilinear_area[0]&&area[2]<bilinear_area[2],"safe area ignored quality footprint");
 std::vector<float>ones(size_t(w)*h*3,1);r=correct(ones,w,h,c,true,nullptr,quality);
 for(unsigned y=unsigned(std::ceil(area[1]*(h-1)));y<=unsigned(std::floor(area[3]*(h-1)));++y)
  for(unsigned x=unsigned(std::ceil(area[0]*(w-1)));x<=unsigned(std::floor(area[2]*(w-1)));++x)
   for(unsigned ch=0;ch<3;++ch)require(r.pixels[(size_t(y)*w+x)*3+ch]==1,"certified area includes lens-introduced invalid sample");
 require(safe_area(c,w,h,false)==std::array<double,4>{0,0,1,1},"off implicitly cropped");
 bool cancelled=false;try{safe_area(c,w,h,true,quality,[]{throw std::runtime_error("cancelled");});}catch(...){cancelled=true;}
 require(cancelled,"domain certification ignored cancellation");
 std::cout<<"PASS real-kernel analytic passband/alias rejection, bounded ringing, green independence, signed HDR and component strength\n";
 }catch(const std::exception&e){std::cerr<<e.what()<<'\n';return 1;}}
