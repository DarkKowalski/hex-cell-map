#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing},
    forward_io::{VertexOutput, FragmentOutput},
    mesh_view_bindings::view,
}
@group(#{MATERIAL_BIND_GROUP}) @binding(100)
var<uniform> settings: vec4<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(101)
var ground_color: texture_2d_array<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102)
var ground_color_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(103)
var ground_detail: texture_2d_array<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(104)
var ground_detail_sampler: sampler;
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
fn projection_weights(n:vec3<f32>)->vec3<f32> {
    let w=pow(abs(n),vec3<f32>(4.0));
    return w/max(dot(w,vec3<f32>(1.0)),0.001);
}
fn albedo(p:vec3<f32>,w:vec3<f32>,layer:i32)->vec3<f32> {
    return textureSample(ground_color,ground_color_sampler,p.zy,layer).rgb*w.x
        +textureSample(ground_color,ground_color_sampler,p.xz,layer).rgb*w.y
        +textureSample(ground_color,ground_color_sampler,p.xy,layer).rgb*w.z;
}
// Normal X/Y, roughness and AO share one linear RGBA array. Transform each
// projection's tangent perturbation into world space before blending.
fn surface_detail(p:vec3<f32>,w:vec3<f32>,layer:i32)->mat2x3<f32> {
    let x=textureSample(ground_detail,ground_detail_sampler,p.zy,layer);
    let y=textureSample(ground_detail,ground_detail_sampler,p.xz,layer);
    let z=textureSample(ground_detail,ground_detail_sampler,p.xy,layer);
    let bump=vec3<f32>(0.0,x.g*2.0-1.0,x.r*2.0-1.0)*w.x
        +vec3<f32>(y.r*2.0-1.0,0.0,y.g*2.0-1.0)*w.y
        +vec3<f32>(z.r*2.0-1.0,z.g*2.0-1.0,0.0)*w.z;
    let packed=x*w.x+y*w.y+z*w.z;
    return mat2x3<f32>(bump,vec3<f32>(packed.b,packed.a,0.0));
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
        var layers=vec4<f32>(0.0);
#ifdef VERTEX_UVS_A
#ifdef VERTEX_UVS_B
        layers=max(vec4<f32>(in.uv,in.uv_b),vec4<f32>(0.0));
#endif
#endif
        let coverage=clamp(dot(layers,vec4<f32>(1.0)),0.0,1.0)*(1.0-in.color.a);
        layers/=max(dot(layers,vec4<f32>(1.0)),0.001);
        let projection=projection_weights(n);
        let warp=vec3<f32>(noise(p.xz*0.7),noise(p.xy*0.6),noise(p.zy*0.8))*0.28;
        var imported_color=vec3<f32>(0.0);
        var imported_bump=vec3<f32>(0.0);
        var roughness=0.0;var occlusion=0.0;
        for(var i=0;i<4;i+=1) {
            let q=(p+warp)*3.2+vec3<f32>(f32(i)*0.37,0.0,f32(i)*0.61);
            imported_color+=albedo(q,projection,i)*layers[i];
            let detail=surface_detail(q,projection,i);
            imported_bump+=detail[0]*layers[i];
            roughness+=detail[1].x*layers[i];occlusion+=detail[1].y*layers[i];
        }
        base=mix(base,imported_color,coverage*0.52);
        base*=0.77+0.23*broad+0.10*medium+0.05*grain+close*0.03*(fine-0.5);
        base*=mix(1.0,0.85+0.15*occlusion,coverage);
        base+=vec3<f32>(0.024,0.008,-0.012)*(broad-0.5);
        let block=abs(fract(p.xz/0.32)-vec2<f32>(0.5));
        let street=smoothstep(0.44,0.49,max(block.x,block.y))*in.color.a;
        base=mix(base,vec3<f32>(0.09,0.095,0.09),street*0.65);
        pbr.material.perceptual_roughness=mix(0.9,clamp(roughness,0.65,1.0),coverage);
        let bump=imported_bump-n*dot(imported_bump,n);
        pbr.N=normalize(n+bump*coverage*(0.12+close*0.08));
    }
    pbr.material.base_color=vec4<f32>(max(base,vec3<f32>(0.005)),1.0);
    pbr.material.metallic=0.0;
    var out:FragmentOutput;
    out.color=apply_pbr_lighting(pbr);out.color=main_pass_post_lighting_processing(pbr,out.color);return out;
}
