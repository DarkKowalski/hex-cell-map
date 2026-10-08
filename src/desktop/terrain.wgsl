#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing},
    forward_io::{VertexOutput, FragmentOutput},
    mesh_view_bindings::view,
}
@group(#{MATERIAL_BIND_GROUP}) @binding(100)
var<uniform> settings: vec4<f32>;
fn hash(p:vec2<f32>)->f32 {
    var q=fract(vec3<f32>(p.x,p.y,p.x)*0.1031);
    q+=vec3<f32>(dot(q,q.yzx+vec3<f32>(33.33)));
    return fract((q.x+q.y)*q.z);
}
fn noise(p:vec2<f32>)->f32 {
    let i=floor(p);let f=fract(p);let u=f*f*(3.0-2.0*f);
    return mix(mix(hash(i),hash(i+vec2<f32>(1.0,0.0)),u.x),mix(hash(i+vec2<f32>(0.0,1.0)),hash(i+vec2<f32>(1.0,1.0)),u.x),u.y);
}
fn triplanar(p:vec3<f32>,n:vec3<f32>,scale:f32)->f32 {
    var w=pow(abs(n),vec3<f32>(4.0));w/=max(dot(w,vec3<f32>(1.0)),0.001);
    return noise(p.yz*scale)*w.x+noise(p.xz*scale)*w.y+noise(p.xy*scale)*w.z;
}
@fragment
fn fragment(in:VertexOutput,@builtin(front_facing) is_front:bool)->FragmentOutput {
    var pbr=pbr_input_from_standard_material(in,is_front);
    let p=in.world_position.xyz;let n=normalize(pbr.N);
    let distance=length(view.world_position-p);
    let broad=triplanar(p,n,0.55);let medium=triplanar(p,n,3.5);let grain=triplanar(p,n,24.0);
    let close=1.0-smoothstep(8.0,45.0,distance);
    var fine=0.5;
    if close>0.01 {fine=triplanar(p,n,180.0);}
    var base=in.color.rgb;
    if settings.y>0.5 {
        let shallow=clamp(in.color.a,0.0,1.0);
        let ripple=noise(p.xz*20.0+vec2<f32>(medium*0.3,0.0));
        base*=0.86+0.14*broad+0.07*ripple;
        base=mix(base,vec3<f32>(0.08,0.18,0.15),shallow*0.25);
        pbr.material.perceptual_roughness=0.32+shallow*0.25;
        pbr.N=normalize(n+vec3<f32>((ripple-0.5)*0.018,0.0,(grain-0.5)*0.018));
    } else {
        let vegetation=smoothstep(0.005,0.065,base.g-base.r);
        let soil=smoothstep(0.50,0.84,medium)*(0.12+vegetation*0.18)*(1.0-in.color.a);
        base=mix(base,vec3<f32>(0.24,0.175,0.10),soil);
        let stone=smoothstep(0.78,0.95,grain)*(1.0-vegetation)*0.14;
        base=mix(base,vec3<f32>(0.31,0.29,0.25),stone);
        base*=0.70+0.30*broad+0.13*medium+0.09*grain+close*0.04*(fine-0.5);
        base+=vec3<f32>(0.024,0.008,-0.012)*(broad-0.5);
        let block=abs(fract(p.xz/0.18)-vec2<f32>(0.5));
        let street=smoothstep(0.44,0.49,max(block.x,block.y))*in.color.a;
        base=mix(base,vec3<f32>(0.09,0.095,0.09),street*0.65);
        pbr.material.perceptual_roughness=clamp(0.79+0.18*grain+0.06*vegetation,0.6,1.0);
        let bump=(0.008+close*0.025)*(1.0-in.color.a*0.8);
        if close>0.05 {
            let dx=triplanar(p+vec3<f32>(0.003,0.0,0.0),n,24.0)-grain;
            let dz=triplanar(p+vec3<f32>(0.0,0.0,0.003),n,24.0)-grain;
            pbr.N=normalize(n+vec3<f32>(-dx*bump,0.0,-dz*bump));
        }
    }
    pbr.material.base_color=vec4<f32>(max(base,vec3<f32>(0.005)),1.0);
    pbr.material.metallic=0.0;
    var out:FragmentOutput;
    out.color=apply_pbr_lighting(pbr);out.color=main_pass_post_lighting_processing(pbr,out.color);return out;
}
