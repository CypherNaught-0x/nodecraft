@group(0) @binding(0) var<storage,read> source:array<u32>;
@group(0) @binding(1) var<storage,read> overlay:array<u32>;
@group(0) @binding(2) var<storage,read> decisions:array<u32>;
@group(0) @binding(3) var<storage,read> params:array<u32>;
@group(0) @binding(4) var<storage,read_write> output:array<u32>;
fn f(i:u32)->f32{return bitcast<f32>(params[i]);}
fn linear(s:f32)->f32{if s<=0.04045{return s/12.92;}return pow((s+0.055)/1.055,2.4);}
fn rgb(i:u32)->vec3<f32>{return vec3<f32>(linear(f(i)),linear(f(i+1u)),linear(f(i+2u)));}
fn decode(p:u32)->vec4<f32>{return vec4<f32>(linear(f32(p&255u)/255.0),linear(f32((p>>8u)&255u)/255.0),linear(f32((p>>16u)&255u)/255.0),f32(p>>24u)/255.0);}
fn encode(v:f32)->u32{let s=clamp(v,0.0,1.0);var e=12.92*s;if s>0.0031308{e=1.055*pow(s,1.0/2.4)-0.055;}return u32(floor(clamp(e,0.0,1.0)*255.0+0.5));}
fn pack(p:vec4<f32>)->u32{let a=u32(floor(clamp(p.a,0.0,1.0)*255.0+0.5));if a==0u{return 0u;}return encode(p.r)|(encode(p.g)<<8u)|(encode(p.b)<<16u)|(a<<24u);}
fn color_pixel(i:u32)->u32{let raw=source[i];var p=decode(raw);if p.a==0.0{p=vec4<f32>(0.0);}let mode=params[4];
 if mode==0u {let a=decisions[2u*i+1u];return select(0u,(raw&0xffffffu)|(a<<24u),a>0u);}
 if mode==1u {let a=decisions[2u*i+1u];p=vec4<f32>(mix(p.rgb,rgb(8u),bitcast<f32>(decisions[2u*i])),f32(a)/255.0);let v=pack(p);return select(0u,(v&0xffffffu)|(a<<24u),a>0u);}
 if mode==2u {let exposure=exp2(f(9u));var c=(p.rgb*vec3<f32>(exp2(f(8u)/2.0),1.0,exp2(-f(8u)/2.0))*exposure-vec3<f32>(0.18))*f(10u)+vec3<f32>(0.18);let y=dot(c,vec3<f32>(0.2126,0.7152,0.0722));p=vec4<f32>(vec3<f32>(y)+f(11u)*(c-vec3<f32>(y)),p.a);}
 if mode==3u {let y=dot(p.rgb,vec3<f32>(0.2126,0.7152,0.0722));var c=p.rgb;let preset=params[5];if preset==1u{c=vec3<f32>(y);}if preset==2u{c=min(vec3<f32>(1.0),vec3<f32>(1.15*y,y,0.8*y));}if preset==3u{c=vec3<f32>(clamp((y-0.18)*1.8+0.18,0.0,1.0));}if preset==4u{c=mix(rgb(12u),rgb(16u),y);}p=vec4<f32>(mix(p.rgb,c,f(8u)),p.a);}
 if mode==4u {let t=rgb(8u);let s=p.rgb;var c=t;let blend=params[5];if blend==1u{c=s*t;}if blend==2u{c=vec3<f32>(1.0)-(vec3<f32>(1.0)-s)*(vec3<f32>(1.0)-t);}if blend==3u{c=select(vec3<f32>(1.0)-2.0*(vec3<f32>(1.0)-s)*(vec3<f32>(1.0)-t),2.0*s*t,s<=vec3<f32>(0.5));}if blend==4u{c=min(s,t);}if blend==5u{c=max(s,t);}if blend==6u{c=abs(s-t);}p=vec4<f32>(mix(s,c,f(12u)*f(11u)),p.a);}
 return pack(p);
}

@group(0) @binding(5) var<storage,read_write> scratch:array<vec4<f32>>;
@group(0) @binding(6) var<storage,read_write> intermediate:array<u32>;
fn premul(p:vec4<f32>)->vec4<f32>{return vec4<f32>(p.rgb*p.a,p.a);}
fn straight(p:vec4<f32>)->vec4<f32>{if p.a>0.0{return vec4<f32>(p.rgb/p.a,p.a);}return vec4<f32>(0.0);}
fn over(a:vec4<f32>,b:vec4<f32>)->vec4<f32>{return a+b*(1.0-a.a);}
fn dims(which:u32)->vec2<u32>{if which==1u{return vec2<u32>(params[18],params[19]);}if which==2u{return vec2<u32>(params[0],params[1]);}return vec2<u32>(params[2],params[3]);}
fn raw_at(pos:vec2<i32>,which:u32,clamped:bool)->u32{
 let d=dims(which);var p=pos;if clamped{p=clamp(p,vec2<i32>(0),vec2<i32>(d)-vec2<i32>(1));}
 if any(p<vec2<i32>(0))||any(p>=vec2<i32>(d)){return 0u;}
 let index=u32(p.y)*d.x+u32(p.x);if which==1u{return overlay[index];}if which==2u{return intermediate[index];}return source[index];
}
fn at(pos:vec2<i32>,which:u32,clamped:bool)->vec4<f32>{return premul(decode(raw_at(pos,which,clamped)));}
fn sample(pos:vec2<f32>,which:u32,clamped:bool)->vec4<f32>{
 let lower=vec2<i32>(floor(pos));let t=fract(pos);
 return mix(mix(at(lower,which,clamped),at(lower+vec2<i32>(1,0),which,clamped),t.x),mix(at(lower+vec2<i32>(0,1),which,clamped),at(lower+vec2<i32>(1,1),which,clamped),t.x),t.y);
}
fn axis_weight(i:i32,start:f32,extent:f32)->f32{
 if extent>1.0{return max(0.0,min(start+extent,f32(i)+1.0)-max(start,f32(i)))/extent;}
 let center=start+extent/2.0-0.5;return max(0.0,1.0-abs(f32(i)-center));
}
fn resized(pos:vec2<f32>,size:vec2<f32>,rect:vec4<f32>,method:u32,which:u32)->vec4<f32>{
 let scale=rect.zw/size;let start=rect.xy+pos*scale;
 if method!=2u{return sample(start+scale/2.0-vec2<f32>(0.5),which,true);}
 let center=start+scale/2.0-vec2<f32>(0.5);
 let lo=vec2<i32>(select(floor(center),floor(start),scale>vec2<f32>(1.0)));
 let hi=vec2<i32>(select(floor(center)+vec2<f32>(1.0),ceil(start+scale)-vec2<f32>(1.0),scale>vec2<f32>(1.0)));
 // Bounded two-axis block reduction avoids a single millions-of-terms f32 chain.
 var sum=vec4<f32>(0.0);var rows=vec4<f32>(0.0);var row_count=0u;
 for(var y=lo.y;y<=hi.y;y++){
  let wy=axis_weight(y,start.y,scale.y);var row=vec4<f32>(0.0);var block=vec4<f32>(0.0);var count=0u;
  for(var x=lo.x;x<=hi.x;x++){
   block+=at(vec2<i32>(x,y),which,true)*axis_weight(x,start.x,scale.x);count++;
   if count==32u{row+=block;block=vec4<f32>(0.0);count=0u;}
  }
  row+=block;rows+=row*wy;row_count++;
  if row_count==32u{sum+=rows;rows=vec4<f32>(0.0);row_count=0u;}
 }
 return sum+rows;
}
fn rect(i:u32)->vec4<f32>{return vec4<f32>(f(i),f(i+1u),f(i+2u),f(i+3u));}
fn geometry(pos:vec2<u32>)->u32{
 let mode=params[4];let i=pos.y*params[0]+pos.x;let ip=vec2<i32>(pos);
 if mode==8u{var q=pos;if params[5]!=1u{q.x=params[2]-1u-pos.x;}if params[5]!=0u{q.y=params[3]-1u-pos.y;}return source[q.y*params[2]+q.x];}
 if mode==9u||(mode==6u&&params[5]==0u){return raw_at(ip+vec2<i32>(rect(24u).xy),0u,false);}
 if mode==10u{if params[5]==0u{return source[decisions[params[0]+pos.y]*params[2]+decisions[pos.x]];}return pack(straight(resized(vec2<f32>(pos),vec2<f32>(f32(params[0]),f32(params[1])),rect(24u),params[5],0u)));}
 if mode==7u{
  var p=vec4<f32>(0.0);let bg=premul(vec4<f32>(rgb(12u),f(15u)));
  if params[7]==1u{let index=decisions[i];var raw=0u;if index<arrayLength(&source){raw=source[index];}if params[32]==1u{return raw;}p=premul(decode(raw));}
  else{let d=vec2<f32>(pos)+vec2<f32>(0.5)-vec2<f32>(f32(params[0]),f32(params[1]))/2.0;let q=vec2<f32>(f(9u)*d.x+f(8u)*d.y,-f(8u)*d.x+f(9u)*d.y)+vec2<f32>(f32(params[2]),f32(params[3]))/2.0-vec2<f32>(0.5);p=sample(q,0u,false);}
  return pack(straight(over(p,bg)));
 }
 if mode==11u{
  let base=at(ip,0u,false);let r=rect(28u);let d=vec2<f32>(pos)-r.xy;if any(d<vec2<f32>(0.0))||any(d>=r.zw){return pack(straight(base));}
  var top=vec4<f32>(0.0);if params[5]==0u{let x=decisions[u32(d.x)];let y=decisions[u32(r.z)+u32(d.y)];top=at(vec2<i32>(i32(x),i32(y)),1u,true);}else{top=resized(d,r.zw,vec4<f32>(0.0,0.0,f32(params[18]),f32(params[19])),params[5],1u);}
  return pack(straight(over(top*f(8u),base)));
 }
 if mode==6u{
  var bg=premul(vec4<f32>(rgb(12u),f(15u)));if params[5]==2u{bg=premul(decode(output[i]));}
  return pack(straight(over(at(ip-vec2<i32>(rect(28u).xy),0u,false),bg)));
 }
 return 0u;
}
fn blur_post(pos:vec2<u32>,blurred:vec4<f32>)->vec4<f32>{
 let original=at(vec2<i32>(pos),0u,false);if params[5]==7u{let d=vec2<f32>(pos)+vec2<f32>(0.5)-vec2<f32>(f(8u),f(9u));let distance=abs(-f(10u)*d.x+f(11u)*d.y)/f32(min(params[2],params[3]));let t=clamp((distance-f(12u)/2.0)/f(13u),0.0,1.0);return mix(original,blurred,t);}
 if params[5]==8u{let i=pos.y*params[0]+pos.x;return mix(original,blurred,bitcast<f32>(decisions[2u*i]));}return blurred;
}
fn halo_index(global_y:i32,x:u32)->u32{
 let y=u32(clamp(global_y,0,i32(params[1])-1));return (y-stage.halo_start)*params[0]+x;
}
fn separable(pos:vec2<u32>,which:u32,phase:u32){
 let radius=i32(params[31]);let w=params[0];
 if params[4]==5u&&params[5]==1u{
  let taps=f32(2*radius+1);
  if phase==0u{
   if pos.x!=0u{return;}let global_y=i32(stage.halo_start+pos.y);var sum=vec4<f32>(0.0);
   for(var dx=-radius;dx<=radius;dx++){sum+=at(vec2<i32>(dx,global_y),which,true);}
   for(var x=0u;x<w;x++){scratch[pos.y*w+x]=sum/taps;if x+1u<w{sum+=at(vec2<i32>(i32(x)+radius+1,global_y),which,true)-at(vec2<i32>(i32(x)-radius,global_y),which,true);}}
  }else{
   if pos.y!=0u{return;}var sum=vec4<f32>(0.0);for(var dy=-radius;dy<=radius;dy++){sum+=scratch[halo_index(i32(stage.start)+dy,pos.x)];}
   for(var y=0u;y<stage.rows;y++){let global_y=stage.start+y;output[global_y*w+pos.x]=pack(straight(sum/taps));if y+1u<stage.rows{sum+=scratch[halo_index(i32(global_y)+radius+1,pos.x)]-scratch[halo_index(i32(global_y)-radius,pos.x)];}}
  }return;
 }
 var sum=vec4<f32>(0.0);
 if phase==0u{
  let global_y=i32(stage.halo_start+pos.y);for(var offset=-radius;offset<=radius;offset++){sum+=at(vec2<i32>(i32(pos.x)+offset,global_y),which,true)*f(64u+u32(offset+radius));}scratch[pos.y*w+pos.x]=sum;
 }else{
  let global_pos=vec2<u32>(pos.x,stage.start+pos.y);for(var offset=-radius;offset<=radius;offset++){sum+=scratch[halo_index(i32(global_pos.y)+offset,pos.x)]*f(64u+u32(offset+radius));}
  let i=global_pos.y*w+pos.x;if params[4]==6u{output[i]=pack(straight(sum));}else{output[i]=pack(straight(blur_post(global_pos,sum)));}
 }
}
fn sampled_blur(pos:vec2<u32>)->u32{
 let kind=params[5];let fp=vec2<f32>(pos);var sum=vec4<f32>(0.0);var total=0.0;
 if kind==5u{
  let center=straight(at(vec2<i32>(pos),0u,false)).rgb;let radius=i32(params[31]);let taps=u32(2*radius+1);
  for(var y=-radius;y<=radius;y++){for(var x=-radius;x<=radius;x++){let pixel=at(vec2<i32>(pos)+vec2<i32>(x,y),0u,true);let delta=(straight(pixel).rgb-center)/f(8u);var w=exp(f(64u+u32(y+radius)*taps+u32(x+radius))-0.5*dot(delta,delta));if x==0&&y==0{w=1.0;}sum+=pixel*w;total+=w;}}
 }else{
  let center=vec2<f32>(f(8u),f(9u));for(var j=0u;j<params[30];j++){var q=fp;
   if kind==2u||kind==6u{q+=vec2<f32>(f(64u+2u*j),f(65u+2u*j));}
   if kind==3u{let d=fp-center;let sn=f(64u+2u*j);let cs=f(65u+2u*j);q=center+vec2<f32>(d.x*cs-d.y*sn,d.x*sn+d.y*cs);}
   if kind==4u{q=center+(fp-center)*f(64u+j);}
   sum+=sample(q,0u,true);total+=1.0;
  }
 }
 if total>0.0{return pack(straight(sum/total));}return pack(straight(at(vec2<i32>(pos),0u,false)));
}
@compute @workgroup_size(8,8) fn main(@builtin(global_invocation_id) id:vec3<u32>){
 if id.x>=params[0]||id.y>=stage.dispatch_rows{return;}let pos=id.xy;let i=pos.y*params[0]+pos.x;let mode=params[4];
 if mode<5u{output[i]=color_pixel(i);return;}
 if mode==5u{if params[5]==0u||params[5]==1u||params[5]>=7u{separable(pos,0u,stage.phase);}else{output[i]=sampled_blur(pos);}return;}
 if mode==6u&&params[5]==2u{
  if stage.phase==0u{intermediate[i]=pack(straight(resized(vec2<f32>(pos),vec2<f32>(f32(params[0]),f32(params[1])),rect(24u),1u,0u)));return;}
  if stage.phase==1u||stage.phase==2u{separable(pos,2u,stage.phase-1u);return;}
 }
 output[i]=geometry(pos);
}

struct Stage {phase:u32,start:u32,rows:u32,halo_start:u32,halo_rows:u32,dispatch_rows:u32,pad0:u32,pad1:u32}
@group(0) @binding(7) var<uniform> stage:Stage;
